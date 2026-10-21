/** 终端「问AI」桥：终端 → AI 助手侧栏（单窗应用，目标恒为本窗 'main'）。
 *  Tauri 事件为进程内投递；浏览器宿主（dev）用 BroadcastChannel 兜底。 */

export interface AiAskPayload {
  /** 去重 id（补发/多端监听时避免重复追加消息） */
  uid: string
  /** 发起提问的终端主机名（帮助 AI 定位上下文） */
  hostName: string
  /** 终端选区文本（发起时已截断） */
  selection: string
  /** 用户的问题 */
  question: string
}

const EVENT = 'ai://ask'
const CHANNEL = 'bgshell.ai-ask'

const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window

/** 终端「问AI」：唤起 AI 对话面板并把选区 + 问题投递过去（目标见文件头注释）。 */
export async function requestAiAsk(payload: Omit<AiAskPayload, 'uid'>): Promise<void> {
  const full: AiAskPayload = { ...payload, uid: crypto.randomUUID() }
  if (isTauri) {
    const { emitTo } = await import('@tauri-apps/api/event')
    // 目标窗的监听若刚挂载（启动中）可能错过首帧，1.2s 后补发一次（uid 相同，面板按 uid 去重）
    await emitTo('main', EVENT, full)
    window.setTimeout(() => {
      void emitTo('main', EVENT, full)
    }, 1200)
    return
  }
  new BroadcastChannel(CHANNEL).postMessage(full)
}

/** 对话面板侧订阅「问AI」（面板常驻挂载，监听始终存活） */
export async function onAiAsk(cb: (p: AiAskPayload) => void): Promise<() => void> {
  if (isTauri) {
    const { getCurrentWebview } = await import('@tauri-apps/api/webview')
    const un = await getCurrentWebview().listen<AiAskPayload>(EVENT, (e) => cb(e.payload))
    return un
  }
  const ch = new BroadcastChannel(CHANNEL)
  ch.onmessage = (e) => cb(e.data as AiAskPayload)
  return () => ch.close()
}
