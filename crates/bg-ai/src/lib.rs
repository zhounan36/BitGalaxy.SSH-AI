//! bg-ai：AI 引擎 crate（BitGalaxy SSH AI 单体应用）。
//! - ai.rs：LLM 对话循环（OpenAI 兼容）+ 审批 UI（卡片弹窗，超时视为拒绝）
//! - toolcore.rs：工具核心（命令分类、只读工具、propose_command、审计）
//! AiHandler 进程级单例语义由宿主壳保证（见 AppDispatcher 组装层）。

pub mod ai;
pub mod toolcore;
