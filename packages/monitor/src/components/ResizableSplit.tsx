import { useEffect, useRef, useState, type ReactNode } from 'react'

interface ResizableSplitProps {
  storageKey: string
  left: ReactNode
  right: ReactNode
  defaultWidthPercent?: number
  minWidthPercent?: number
  maxWidthPercent?: number
}

export default function ResizableSplit({
  storageKey,
  left,
  right,
  defaultWidthPercent = 50,
  minWidthPercent = 20,
  maxWidthPercent = 80,
}: ResizableSplitProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [widthPercent, setWidthPercent] = useState<number>(() => {
    try {
      const saved = localStorage.getItem(storageKey)
      const n = saved ? parseFloat(saved) : NaN
      if (Number.isFinite(n) && n >= minWidthPercent && n <= maxWidthPercent)
        return n
    } catch {}
    return defaultWidthPercent
  })
  const dragging = useRef(false)

  useEffect(() => {
    try {
      localStorage.setItem(storageKey, String(widthPercent))
    } catch {}
  }, [storageKey, widthPercent])

  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!dragging.current || !containerRef.current) return
      const rect = containerRef.current.getBoundingClientRect()
      const next = ((e.clientX - rect.left) / rect.width) * 100
      const clamped = Math.max(minWidthPercent, Math.min(maxWidthPercent, next))
      setWidthPercent(clamped)
    }
    function onUp() {
      if (!dragging.current) return
      dragging.current = false
      document.body.style.cursor = ''
      document.body.style.userSelect = ''
    }
    window.addEventListener('mousemove', onMove)
    window.addEventListener('mouseup', onUp)
    return () => {
      window.removeEventListener('mousemove', onMove)
      window.removeEventListener('mouseup', onUp)
    }
  }, [minWidthPercent, maxWidthPercent])

  return (
    <div ref={containerRef} className="flex h-full min-h-0 w-full">
      <div style={{ width: `${widthPercent}%` }} className="min-w-0 min-h-0">
        {left}
      </div>
      <div
        role="separator"
        aria-orientation="vertical"
        onMouseDown={(e) => {
          e.preventDefault()
          dragging.current = true
          document.body.style.cursor = 'col-resize'
          document.body.style.userSelect = 'none'
        }}
        onDoubleClick={() => setWidthPercent(defaultWidthPercent)}
        title="Drag to resize · double-click to reset"
        className="group relative w-2 shrink-0 cursor-col-resize flex items-center justify-center"
      >
        <span className="h-12 w-[3px] rounded-full bg-border group-hover:bg-primary/60 transition-colors" />
      </div>
      <div
        style={{ width: `${100 - widthPercent}%` }}
        className="min-w-0 min-h-0"
      >
        {right}
      </div>
    </div>
  )
}
