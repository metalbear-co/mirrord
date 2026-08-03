const JSON_TOKEN_RE =
  /("(?:\\.|[^"\\])*"\s*:)|("(?:\\.|[^"\\])*")|\b(true|false|null)\b|(-?\d+(?:\.\d+)?(?:[eE][+-]?\d+)?)/g

export default function JsonHighlight({ value }: { value: unknown }) {
  const text = JSON.stringify(value ?? null, null, 2)
  const parts: { kind: string; text: string; start: number }[] = []
  let last = 0
  for (const match of text.matchAll(JSON_TOKEN_RE)) {
    const idx = match.index
    if (idx > last)
      parts.push({ kind: 'plain', text: text.slice(last, idx), start: last })
    if (match[1]) parts.push({ kind: 'key', text: match[1], start: idx })
    else if (match[2])
      parts.push({ kind: 'string', text: match[2], start: idx })
    else if (match[3])
      parts.push({ kind: 'literal', text: match[3], start: idx })
    else if (match[4])
      parts.push({ kind: 'number', text: match[4], start: idx })
    last = idx + match[0].length
  }
  if (last < text.length)
    parts.push({ kind: 'plain', text: text.slice(last), start: last })

  return (
    <pre className="text-meta whitespace-pre-wrap break-all font-mono leading-relaxed">
      <code data-language="json">
        {parts.map((p) => {
          if (p.kind === 'key')
            return (
              <span key={p.start} className="text-sky-500 dark:text-sky-300">
                {p.text}
              </span>
            )
          if (p.kind === 'string')
            return (
              <span
                key={p.start}
                className="text-emerald-600 dark:text-emerald-300"
              >
                {p.text}
              </span>
            )
          if (p.kind === 'number')
            return (
              <span
                key={p.start}
                className="text-amber-600 dark:text-amber-300"
              >
                {p.text}
              </span>
            )
          if (p.kind === 'literal')
            return (
              <span
                key={p.start}
                className="font-semibold text-fuchsia-600 dark:text-fuchsia-300"
              >
                {p.text}
              </span>
            )
          return <span key={p.start}>{p.text}</span>
        })}
      </code>
    </pre>
  )
}
