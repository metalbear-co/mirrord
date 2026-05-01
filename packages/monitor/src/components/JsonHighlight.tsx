const JSON_TOKEN_RE =
  /("(?:\\.|[^"\\])*"\s*:)|("(?:\\.|[^"\\])*")|\b(true|false|null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g

export default function JsonHighlight({ value }: { value: unknown }) {
  const text = JSON.stringify(value, null, 2)
  const parts: Array<{ kind: string; text: string }> = []
  let last = 0
  for (const match of text.matchAll(JSON_TOKEN_RE)) {
    const idx = match.index ?? 0
    if (idx > last) parts.push({ kind: 'plain', text: text.slice(last, idx) })
    if (match[1]) parts.push({ kind: 'key', text: match[1] })
    else if (match[2]) parts.push({ kind: 'string', text: match[2] })
    else if (match[3]) parts.push({ kind: 'literal', text: match[3] })
    else if (match[4]) parts.push({ kind: 'number', text: match[4] })
    last = idx + match[0].length
  }
  if (last < text.length) parts.push({ kind: 'plain', text: text.slice(last) })

  return (
    <pre className="text-[11px] whitespace-pre-wrap break-all bg-card/30 border border-border rounded-md p-3 font-mono leading-relaxed">
      <code data-language="json">
        {parts.map((p, i) => {
          if (p.kind === 'key')
            return (
              <span key={i} className="text-sky-500 dark:text-sky-300">
                {p.text}
              </span>
            )
          if (p.kind === 'string')
            return (
              <span key={i} className="text-emerald-600 dark:text-emerald-300">
                {p.text}
              </span>
            )
          if (p.kind === 'number')
            return (
              <span key={i} className="text-amber-600 dark:text-amber-300">
                {p.text}
              </span>
            )
          if (p.kind === 'literal')
            return (
              <span
                key={i}
                className="text-fuchsia-600 dark:text-fuchsia-300 font-semibold"
              >
                {p.text}
              </span>
            )
          return <span key={i}>{p.text}</span>
        })}
      </code>
    </pre>
  )
}
