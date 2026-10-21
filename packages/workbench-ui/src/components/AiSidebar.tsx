import { useEffect, useState } from 'react'
import { aiSocket } from '../api/ws'
import { onAiAsk, type AiAskPayload } from '../api/aiBridge'
import { ChatPanel } from './ChatPanel'

/** AI 助手侧栏（同进程整合，AI 永远在线）：
 *  - 复用 ChatPanel 全部对话能力（流式/工具卡片/历史），经 aiSocket 直连
 *    本进程 AI 引擎（与 SSH 数据面同端口 7780）
 *  - 自带右下角 FAB 开关与「问AI」监听（终端选区提问自动唤起本侧栏）
 */
export function AiSidebar() {
  const [open, setOpen] = useState(false)
  const [ask, setAsk] = useState<AiAskPayload | null>(null)

  // 终端「问AI」（本窗事件）：唤起侧栏并投递给 ChatPanel（uid 去重在其内部）
  useEffect(() => {
    let disposed = false
    let off: (() => void) | undefined
    void onAiAsk((p) => {
      if (disposed || !p?.uid) return
      setOpen(true)
      setAsk(p)
    }).then((u) => {
      if (disposed) u()
      else off = u
    })
    return () => {
      disposed = true
      off?.()
    }
  }, [])

  if (!open) {
    return (
      <button type="button" className="ai-fab" title="打开 AI 助手" onClick={() => setOpen(true)}>
        AI
      </button>
    )
  }
  // .wb-chat-wrap：右下角浮层定位（index.css 含该规则）
  return (
    <div className="wb-chat-wrap">
      <ChatPanel open onClose={() => setOpen(false)} socket={aiSocket} incomingAsk={ask} />
    </div>
  )
}
