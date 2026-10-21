import { useRef, useState, type PointerEvent as ReactPointerEvent, type RefObject } from 'react'

/** 浮层位置拖动（AI 对话面板 / AI 候选命令面板共用）：
 *  按住标题栏拖动移动浮层位置；拖动中直接改 DOM（left/top 并清 right/bottom，
 *  绕开 state 避免整面板每帧重渲染），松手才落 state + localStorage 持久化。
 *  未拖动过时保持 CSS 默认右下角定位。标题栏上的按钮/输入框按下不触发拖动。 */

export interface FloatPos {
  x: number
  y: number
}

const PREFIX = 'bgsh-float-pos.'

/** 视口内夹取：至少留出一截标题栏可再抓到（分辨率/窗口变小后位置仍可用） */
function clampPos(p: FloatPos): FloatPos {
  const maxX = Math.max(0, window.innerWidth - 120)
  const maxY = Math.max(0, window.innerHeight - 36)
  return {
    x: Math.min(Math.max(0, Math.round(p.x)), maxX),
    y: Math.min(Math.max(0, Math.round(p.y)), maxY),
  }
}

function loadFloatPos(key: string): FloatPos | undefined {
  try {
    const raw = localStorage.getItem(PREFIX + key)
    if (!raw) return undefined
    const p = JSON.parse(raw) as Partial<FloatPos>
    if (typeof p.x !== 'number' || typeof p.y !== 'number') return undefined
    return clampPos({ x: p.x, y: p.y })
  } catch {
    return undefined
  }
}

function saveFloatPos(key: string, p: FloatPos) {
  try {
    localStorage.setItem(PREFIX + key, JSON.stringify(p))
  } catch {
    /* 存储不可用时位置只在本次生效 */
  }
}

/** 绑定到浮层标题栏的 pointer 事件（按下捕获指针，move/up/cancel 收尾） */
export function useFloatDrag(key: string, elRef: RefObject<HTMLDivElement | null>) {
  const [pos, setPos] = useState<FloatPos | undefined>(() => loadFloatPos(key))
  const dragRef = useRef<{
    x: number
    y: number
    ox: number
    oy: number
    moved: boolean
    last?: FloatPos
  } | null>(null)

  const onPointerDown = (e: ReactPointerEvent<HTMLElement>) => {
    if (e.button !== 0) return
    // 标题栏上的按钮/输入不触发拖动（点击行为不受影响）
    if ((e.target as HTMLElement).closest('button, input, a, textarea')) return
    const el = elRef.current
    if (!el) return
    const r = el.getBoundingClientRect()
    dragRef.current = { x: e.clientX, y: e.clientY, ox: r.left, oy: r.top, moved: false }
    e.currentTarget.setPointerCapture(e.pointerId)
  }

  const onPointerMove = (e: ReactPointerEvent<HTMLElement>) => {
    const d = dragRef.current
    const el = elRef.current
    if (!d || !el) return
    const dx = e.clientX - d.x
    const dy = e.clientY - d.y
    // 位移 4px 内视为点击而非拖动，避免轻微抖动误判
    if (!d.moved && Math.abs(dx) + Math.abs(dy) < 4) return
    d.moved = true
    d.last = clampPos({ x: d.ox + dx, y: d.oy + dy })
    el.style.left = `${d.last.x}px`
    el.style.top = `${d.last.y}px`
    el.style.right = 'auto'
    el.style.bottom = 'auto'
  }

  const onPointerEnd = (e: ReactPointerEvent<HTMLElement>) => {
    const d = dragRef.current
    if (!d) return
    dragRef.current = null
    if (e.currentTarget.hasPointerCapture(e.pointerId)) e.currentTarget.releasePointerCapture(e.pointerId)
    if (!d.moved || !d.last) return
    setPos(d.last)
    saveFloatPos(key, d.last)
  }

  return {
    /** 持久化的浮层位置（undefined = 未拖动过，保持 CSS 默认定位） */
    pos,
    /** 展开到浮层根节点的定位样式（有位置时覆盖右下角默认） */
    posStyle: pos ? { left: pos.x, top: pos.y, right: 'auto', bottom: 'auto' } : undefined,
    /** 展开到标题栏元素上 */
    headBind: {
      onPointerDown,
      onPointerMove,
      onPointerUp: onPointerEnd,
      onPointerCancel: onPointerEnd,
    },
  }
}
