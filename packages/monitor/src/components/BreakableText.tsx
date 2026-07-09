import { Fragment } from 'react'

// Values in the monitor are mostly URLs, paths, and MIME types with no spaces to wrap at.
// Rather than break-all (…applicatio↵n/json), insert soft break opportunities after their
// natural delimiters so a wrapped value only ever breaks at a slash, dot, dash or separator;
// values with no delimiters still wrap anywhere rather than overflowing.
export default function BreakableText({ text }: { text: string }) {
  return (
    <span className="[overflow-wrap:anywhere]">
      {text.split(/(?<=[/.?&=_-])/).map((part, i) => (
        <Fragment key={i}>
          {part}
          <wbr />
        </Fragment>
      ))}
    </span>
  )
}
