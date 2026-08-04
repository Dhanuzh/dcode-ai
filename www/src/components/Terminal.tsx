import { useEffect, useRef, useState } from 'react'
import { DEMO, type Frame } from '../data'

const TONE: Record<Frame['kind'], string> = {
  user: 'text-accent font-semibold',
  think: 'text-muted italic',
  tool: 'text-violet',
  out: 'text-muted',
  text: 'text-fg',
  rule: 'text-muted',
}

const PREFIX: Record<Frame['kind'], string> = {
  user: '› ',
  think: '• ',
  tool: '● ',
  out: '  └ ',
  text: '  ',
  rule: '',
}

/**
 * Replays a dcode-ai session line by line. Purely decorative: it respects
 * `prefers-reduced-motion` by rendering the finished transcript immediately.
 */
export function Terminal() {
  const [shown, setShown] = useState(0)
  const timer = useRef<number | undefined>(undefined)

  useEffect(() => {
    const reduced = window.matchMedia('(prefers-reduced-motion: reduce)').matches
    if (reduced) {
      setShown(DEMO.length)
      return
    }

    const tick = () => {
      setShown((n) => {
        if (n >= DEMO.length) {
          // Hold the completed transcript, then loop.
          timer.current = window.setTimeout(() => setShown(0), 4200)
          return n
        }
        timer.current = window.setTimeout(tick, DEMO[n].kind === 'text' ? 420 : 620)
        return n + 1
      })
    }
    timer.current = window.setTimeout(tick, 500)
    return () => window.clearTimeout(timer.current)
  }, [])

  return (
    <div className="overflow-hidden rounded-xl border border-line bg-surface shadow-2xl shadow-black/60">
      {/* Title bar */}
      <div className="flex items-center gap-2 border-b border-line bg-raised px-4 py-3">
        <span className="h-3 w-3 rounded-full bg-red/80" />
        <span className="h-3 w-3 rounded-full bg-amber/80" />
        <span className="h-3 w-3 rounded-full bg-green/80" />
        <span className="ml-2 font-mono text-xs text-muted">dcode-ai — ~/projects/api</span>
      </div>

      {/* Transcript */}
      <div className="min-h-[19rem] p-4 font-mono text-[13px] leading-relaxed sm:min-h-[21rem] sm:p-5">
        {DEMO.slice(0, shown).map((frame, i) => {
          if (frame.kind === 'rule') {
            return (
              <div key={i} className="mt-3 flex items-center gap-2 text-muted">
                <span className="whitespace-nowrap text-xs">─ {frame.text} </span>
                <span className="h-px flex-1 bg-line" />
              </div>
            )
          }
          const last = i === shown - 1 && shown < DEMO.length
          return (
            <div
              key={i}
              className={`rise ${TONE[frame.kind]} ${frame.kind === 'user' ? 'mb-2' : ''} ${last ? 'caret' : ''}`}
            >
              <span className="text-muted">{PREFIX[frame.kind]}</span>
              {frame.text}
            </div>
          )
        })}
      </div>

      {/* Status bar — mirrors the real TUI's bottom line */}
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t border-line bg-raised px-4 py-2 font-mono text-[11px] text-muted">
        <span className="text-green">● idle</span>
        <span className="text-line">·</span>
        <span className="text-fg">dcode-ai</span>
        <span className="text-line">·</span>
        <span className="text-violet">@build</span>
        <span className="text-line">·</span>
        <span>ctx 12%</span>
        <span className="ml-auto text-accent">claude-sonnet-4-6</span>
      </div>
    </div>
  )
}
