import { useEffect, useRef, useState, type ReactNode } from 'react'

interface ResizableSplitProps {
  storageKey: string
  left: ReactNode
  right: ReactNode
  defaultPct?: number
  minPct?: number
  maxPct?: number
}

export default function ResizableSplit({
  storageKey,
  left,
  right,
  defaultPct = 50,
  minPct = 20,
  maxPct = 80,
}: ResizableSplitProps) {
  const containerRef = useRef<HTMLDivElement>(null)
  const [pct, setPct] = useState<number>(() => {
    try {
      const saved = localStorage.getItem(storageKey)
      const n = saved ? parseFloat(saved) : NaN
      if (Number.isFinite(n) && n >= minPct && n <= maxPct) return n
    } catch {}
    return defaultPct
  })
  const dragging = useRef(false)

  useEffect(() => {
    try {
      localStorage.setItem(storageKey, String(pct))
    } catch {}
  }, [storageKey, pct])

  useEffect(() => {
    function onMove(e: MouseEvent) {
      if (!dragging.current || !containerRef.current) return
      const rect = containerRef.current.getBoundingClientRect()
      const next = ((e.clientX - rect.left) / rect.width) * 100
      const clamped = Math.max(minPct, Math.min(maxPct, next))
      setPct(clamped)
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
  }, [minPct, maxPct])

  return (
    <div ref={containerRef} className="flex h-full min-h-0 w-full">
      <div style={{ width: `${pct}%` }} className="min-w-0 min-h-0">
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
        onDoubleClick={() => setPct(defaultPct)}
        title="Drag to resize · double-click to reset"
        className="group relative w-2 shrink-0 cursor-col-resize flex items-center justify-center"
      >
        <span className="h-12 w-[3px] rounded-full bg-border group-hover:bg-primary/60 transition-colors" />
      </div>
      <div style={{ width: `${100 - pct}%` }} className="min-w-0 min-h-0">
        {right}
      </div>
    </div>
  )
}
