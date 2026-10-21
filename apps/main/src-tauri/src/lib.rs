//! BitGalaxy SSH AI（单体应用，Tauri 2）：
//! - SSH 终端 / SFTP 文件管理（含拖出下载 /dl）/ 运行状态 / 主机档案管理
//! - AI 助手同进程整合：AiHandler 进程级单例 + OutHub 广播，AI 与用户共用
//!   同一 WS 端口；AI 工具集只读 + propose_command——AI 只把候选命令加入
//!   命令列表，用户自己点击执行，AI 从不直接执行任何命令
//! - 单 WS 端口 7780（BGSH_PORT 环境变量覆盖），SSH 会话与 AI 只读工具
//!   共用同一 ConnRegistry 连接池

use std::path::PathBuf;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use tauri::Manager;

use bg_ai::ai::AiHandler;
use bg_core::policy::AiPolicy;
use bg_core::protocol::{
    self, AgentOp, AgentResult, ClientFrame, ServerFrame, SettingsResultM, DF_SFTP_UPLOAD, DF_STDIN,
};
use bg_core::stores::Stores;
use bg_core::wire::{OutHub, Outbound};
use bg_server::{ConnHandler, Dispatcher, WsServer};
use bg_ssh::dl_http;
use bg_ssh::pool::ConnRegistry;
use bg_ssh::session::SessionManager;
use tokio::net::TcpStream;

/// 默认端口：7780（独立于 BitGalaxy Shell 四 EXE 的 7777/7771/7772/7773，可共存）
const DEFAULT_PORT: u16 = 7780;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// 数据目录：%APPDATA%\bitgalaxy-ssh-ai（独立产品，独立数据；hosts/settings/chat 按域拆库）
pub fn resolve_data_dir() -> PathBuf {
    if let Ok(appdata) = std::env::var("APPDATA") {
        return PathBuf::from(appdata).join("bitgalaxy-ssh-ai");
    }
    let home = std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .unwrap_or_default();
    PathBuf::from(home).join(".local").join("state").join("bitgalaxy-ssh-ai")
}

pub fn append_log(data_dir: &PathBuf, msg: &str) {
    let line = format!("{} {msg}\n", chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
    let _ = std::fs::create_dir_all(data_dir);
    let _ = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(data_dir.join("app.log"))
        .and_then(|mut f| std::io::Write::write_all(&mut f, line.as_bytes()));
}

// ── Tauri IPC 命令（终端/文件面板上传：本地文件元信息与分块读取）────────────

/// 文件元信息（拖拽上传面板：展示大小 / 跳过目录）
#[derive(serde::Serialize)]
struct FileStat {
    name: String,
    size: u64,
    is_dir: bool,
}

#[tauri::command]
fn stat_file(path: String) -> Result<FileStat, String> {
    let md = std::fs::metadata(&path).map_err(|e| e.to_string())?;
    let name = std::path::Path::new(&path)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.clone());
    Ok(FileStat {
        name,
        size: md.len(),
        is_dir: md.is_dir(),
    })
}

/// 分块读取本地文件（终端拖拽上传：前端按 256KB 拉取，经 IPC 二进制响应返回）
#[tauri::command]
fn read_file_chunk(path: String, offset: u64, len: u32) -> Result<tauri::ipc::Response, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(&path).map_err(|e| e.to_string())?;
    f.seek(SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
    let mut buf = vec![0u8; len as usize];
    let mut total = 0;
    while total < buf.len() {
        match f.read(&mut buf[total..]) {
            Ok(0) => break,
            Ok(n) => total += n,
            Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    buf.truncate(total);
    Ok(tauri::ipc::Response::new(buf))
}

/// Agent 自身控制（设置弹窗）：版本信息 / 退出
type AgentCtl = Arc<dyn Fn(AgentOp) -> BoxFuture<'static, AgentResult> + Send + Sync>;

fn make_agent_ctl(app: tauri::AppHandle, data_dir: PathBuf) -> AgentCtl {
    Arc::new(move |op: AgentOp| {
        let app = app.clone();
        let data_dir = data_dir.clone();
        Box::pin(async move {
            match op.op.as_str() {
                "get-info" => AgentResult {
                    op: "get-info".into(),
                    ok: true,
                    autostart: None,
                    open_on_start: None,
                    version: Some(VERSION.into()),
                    message: None,
                },
                "shutdown" => {
                    append_log(&data_dir, "[app] 收到退出指令，进程退出");
                    let app = app.clone();
                    tokio::spawn(async move {
                        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                        app.exit(0);
                    });
                    AgentResult {
                        op: "shutdown".into(),
                        ok: true,
                        autostart: None,
                        open_on_start: None,
                        version: None,
                        message: None,
                    }
                }
                other => AgentResult {
                    op: other.into(),
                    ok: false,
                    autostart: None,
                    open_on_start: None,
                    version: None,
                    message: Some("未知操作".into()),
                },
            }
        })
    })
}

/// 应用分发器：SSH 连接注册表 + 拖出下载令牌表 + AI 处理器（进程级单例，跨 WS 连接共享）
struct AppDispatcher {
    stores: Stores,
    registry: Arc<ConnRegistry>,
    ai: Arc<AiHandler>,
    dl: Arc<dl_http::DlTickets>,
    agent_ctl: AgentCtl,
}

impl AppDispatcher {
    fn new(port: u16, stores: Stores, policy: AiPolicy, agent_ctl: AgentCtl) -> Self {
        // SSH 会话与 AI 只读工具（list_dir/read_file/get_metrics）共用同一连接池
        let registry = Arc::new(ConnRegistry::new(stores.clone(), policy.limits.idle_close_sec));
        let dl = Arc::new(dl_http::DlTickets::new(port));
        // AiHandler 进程级单例：审批卡片 / 对话状态跨 WS 连接（断线重连后的新连接）
        // 依然路由到同一份登记表；AI 帧经 OutHub 广播到所有活跃连接
        let out_hub = OutHub::default();
        let ai = Arc::new(AiHandler::new(stores.clone(), out_hub, policy, registry.clone()));
        AppDispatcher { stores, registry, ai, dl, agent_ctl }
    }
}

impl Dispatcher for AppDispatcher {
    /// /dl 请求（文件面板拖出下载）在同端口分流：消费整条连接；其余（WS 握手等）不消费字节
    fn try_take_stream<'a>(&'a self, stream: &'a mut TcpStream) -> BoxFuture<'a, bool> {
        let dl = &self.dl;
        let registry = &self.registry;
        Box::pin(async move { dl_http::try_handle(stream, dl, registry).await })
    }

    fn on_conn_open(&self, out: Outbound) -> BoxFuture<'static, Box<dyn ConnHandler>> {
        // 本连接加入 AI 广播组（断开时由 OutHub 发送路径自动清理）
        self.ai.register_conn(out.clone());
        let manager = SessionManager::new(
            self.stores.clone(),
            out.clone(),
            self.registry.clone(),
            self.dl.clone(),
        );
        let conn = AppConn {
            manager,
            stores: self.stores.clone(),
            ai: self.ai.clone(),
            agent_ctl: self.agent_ctl.clone(),
            out,
        };
        Box::pin(async move { Box::new(conn) as Box<dyn ConnHandler> })
    }
}

/// 连接级状态：SSH 会话管理器（断开清空本连接全部会话，连接由注册表按空闲回收）
struct AppConn {
    manager: SessionManager,
    stores: Stores,
    ai: Arc<AiHandler>,
    agent_ctl: AgentCtl,
    out: Outbound,
}

impl Drop for AppConn {
    fn drop(&mut self) {
        self.manager.close_all("socket-closed");
    }
}

impl ConnHandler for AppConn {
    fn dispatch(&self, frame: ClientFrame) -> BoxFuture<'static, ()> {
        let manager = self.manager.clone();
        let stores = self.stores.clone();
        let ai = self.ai.clone();
        let agent_ctl = self.agent_ctl.clone();
        let out = self.out.clone();
        Box::pin(async move {
            match frame {
                // ── AI（同进程整合：对话/审批/历史）──
                ClientFrame::Ai { m } => ai.handle_ai(m),
                ClientFrame::AiApprove { m } => ai.handle_approve(&m),
                // ── SSH 数据面 ──
                ClientFrame::Open { s, m } => {
                    manager.open(&s, &m.host_id, &m);
                }
                ClientFrame::Resize { s, m } => {
                    // resize 在独立任务执行：写挂起（僵尸连接）不得阻塞 WS 帧分发循环
                    tokio::spawn(async move {
                        manager.resize(&s, m.cols, m.rows).await;
                    });
                }
                ClientFrame::Close { s, m } => {
                    let reason = m.reason.unwrap_or_else(|| "user".into());
                    manager.close(&s, &reason);
                }
                ClientFrame::ConfirmHostKey { s, m } => manager.resolve_host_key(&s, m.accepted),
                ClientFrame::RetryAuth { s, m } => manager.resolve_auth(&s, m.password),
                ClientFrame::Hosts { m } => manager.handle_hosts_op(&m),
                ClientFrame::Sftp { s, m } => manager.handle_sftp(&s, &m),
                ClientFrame::Metrics { s, m } => {
                    manager.set_metrics_interval(&s, m.interval_sec.unwrap_or(0));
                }
                ClientFrame::Probe { m } => {
                    tokio::spawn(async move {
                        manager.probe(&m.host_id).await;
                    });
                }
                ClientFrame::TestHost { m } => {
                    tokio::spawn(async move {
                        manager.test_host(m).await;
                    });
                }
                // ── 设置 / 自身控制 ──
                ClientFrame::Agent { m } => {
                    tokio::spawn(async move {
                        let result = agent_ctl(m).await;
                        out.send_frame(&ServerFrame::AgentResult { m: result });
                    });
                }
                ClientFrame::Settings { m } => {
                    let m = handle_settings(&stores, &m);
                    out.send_frame(&ServerFrame::SettingsResult { m });
                }
                // SQL / 串口帧静默忽略（本应用不承载）
                _ => {}
            }
        })
    }

    fn dispatch_data(&self, df: u8, sid: &str, payload: Vec<u8>) -> BoxFuture<'static, ()> {
        let manager = self.manager.clone();
        let sid = sid.to_string();
        Box::pin(async move {
            match df {
                DF_STDIN => {
                    // 入队即返回（会话 stdin 任务异步写入），分发循环永不被 SSH 写阻塞
                    manager.try_input(&sid, &payload);
                }
                DF_SFTP_UPLOAD => {
                    if let Some((op_id, chunk)) = protocol::decode_op_payload(&payload) {
                        manager.upload_chunk(&sid, &op_id, chunk.to_vec());
                    }
                }
                _ => {}
            }
        })
    }
}

/// settings 键值操作（m: {op, key?, value?}）：AI 配置（ai.*）等全库直通
fn handle_settings(stores: &Stores, m: &serde_json::Value) -> SettingsResultM {
    let op = m.get("op").and_then(|v| v.as_str()).unwrap_or_default();
    match op {
        "list" => SettingsResultM {
            ok: true,
            items: Some(stores.settings.get_all()),
            value: None,
            message: None,
        },
        "get" => {
            let key = m.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            SettingsResultM {
                ok: true,
                items: None,
                value: Some(stores.settings.get(key).unwrap_or(serde_json::Value::Null)),
                message: None,
            }
        }
        "set" => {
            let key = m.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            let value = m.get("value").cloned().unwrap_or(serde_json::Value::Null);
            stores.settings.set(key, &value);
            SettingsResultM { ok: true, items: None, value: None, message: None }
        }
        "delete" => {
            let key = m.get("key").and_then(|v| v.as_str()).unwrap_or_default();
            SettingsResultM {
                ok: stores.settings.delete(key),
                items: None,
                value: None,
                message: None,
            }
        }
        _ => SettingsResultM {
            ok: false,
            items: None,
            value: None,
            message: Some("未知 settings 操作".into()),
        },
    }
}

pub fn run() {
    tauri::Builder::default()
        // 桌面单实例：二次启动唤起已有主窗
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            if let Some(win) = app.get_webview_window("main") {
                let _ = win.show();
                let _ = win.set_focus();
            }
        }))
        // 终端复制/粘贴：原生剪贴板读写（经 plugin:clipboard-manager IPC）
        .plugin(tauri_plugin_clipboard_manager::init())
        // 原生文件选择器（主机编辑：私钥文件路径浏览）
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(tauri::generate_handler![stat_file, read_file_chunk])
        .setup(move |app| {
            let data_dir = resolve_data_dir();
            let _ = std::fs::create_dir_all(&data_dir);
            append_log(&data_dir, &format!("[app] 启动 v{VERSION}（BitGalaxy SSH AI）"));

            let port = std::env::var("BGSH_PORT")
                .ok()
                .and_then(|p| p.parse().ok())
                .unwrap_or(DEFAULT_PORT);

            // 持久层（按域拆库：hosts/settings/chat）
            let stores = Stores::open_domains(&data_dir);
            let policy = bg_core::policy::load_ai_policy(&data_dir, &stores);

            let agent_ctl = make_agent_ctl(app.handle().clone(), data_dir);
            let dispatcher = Arc::new(AppDispatcher::new(port, stores, policy, agent_ctl));
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
            tauri::async_runtime::spawn(async move {
                if let Err(e) = WsServer::start(port, dispatcher, Some(ready_tx)).await {
                    eprintln!("[app] WS 服务启动失败: {e}");
                }
            });

            // WS 就绪后展示主窗。conf 中 visible=false：先服务后窗口，避免前端连不上 7780
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                if ready_rx.await.is_ok() {
                    if let Some(win) = app_handle.get_webview_window("main") {
                        let _ = win.show();
                        let _ = win.set_focus();
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("BitGalaxy SSH AI 启动失败");
}
