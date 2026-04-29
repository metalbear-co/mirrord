import { Box, KeyRound, User } from 'lucide-react'
import type { OperatorSessionSummary } from '../types'

interface OperatorSessionDetailProps {
  session: OperatorSessionSummary
}

function relativeTime(iso: string): string {
  const t = new Date(iso).getTime()
  if (!Number.isFinite(t)) return ''
  const diff = (Date.now() - t) / 1000
  if (diff < 60) return `${Math.max(0, Math.floor(diff))}s ago`
  if (diff < 3600) return `${Math.floor(diff / 60)}m ago`
  if (diff < 86400) return `${Math.floor(diff / 3600)}h ago`
  return `${Math.floor(diff / 86400)}d ago`
}

function describeFilter(f: OperatorSessionSummary['httpFilter']): string {
  if (!f) return 'No filter'
  if (f.headerFilter) return `header: ${f.headerFilter}`
  if (f.pathFilter) return `path: ${f.pathFilter}`
  if (f.allOf?.length) return `${f.allOf.length} filters (all)`
  if (f.anyOf?.length) return `${f.anyOf.length} filters (any)`
  return 'Custom filter'
}

export default function OperatorSessionDetail({ session }: OperatorSessionDetailProps) {
  const targetLabel = session.target
    ? `${session.target.kind}/${session.target.name}`
    : 'targetless'
  return (
    <div className="h-full overflow-y-auto p-6 max-w-3xl">
      <div className="flex items-center gap-3 mb-6">
        <Box className="h-5 w-5 text-muted-foreground" />
        <div>
          <h1 className="text-xl font-bold font-mono leading-none">{targetLabel}</h1>
          <div className="text-xs text-muted-foreground mt-1">
            {session.namespace} · started {relativeTime(session.createdAt)}
          </div>
        </div>
        <span className="ml-auto text-[11px] uppercase tracking-wider font-semibold text-muted-foreground">
          read-only
        </span>
      </div>

      <div className="grid grid-cols-2 gap-3">
        <Card title="Owner" icon={<User className="h-4 w-4" />}>
          <div className="text-sm font-medium">{session.owner.username}</div>
          <div className="text-[11px] text-muted-foreground font-mono mt-0.5 break-all">
            {session.owner.k8sUsername}
          </div>
        </Card>
        <Card title="Session key" icon={<KeyRound className="h-4 w-4" />}>
          <div className="text-sm font-mono font-medium">{session.key}</div>
          <div className="text-[11px] text-muted-foreground mt-0.5">id {session.id}</div>
        </Card>
        <Card title="HTTP filter" className="col-span-2">
          <div className="text-xs font-mono break-all">{describeFilter(session.httpFilter)}</div>
        </Card>
        {session.target?.container && (
          <Card title="Container" className="col-span-2">
            <div className="text-xs font-mono">{session.target.container}</div>
          </Card>
        )}
      </div>

      <p className="text-xs text-muted-foreground mt-6">
        This session belongs to a teammate. Use the{' '}
        <a
          href="https://chromewebstore.google.com/detail/mirrord/bijejadnnfgjkfdocgocklekjhnhkhkf"
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          mirrord browser extension
        </a>{' '}
        to inject the matching header into your browser traffic and ride
        along.
      </p>
    </div>
  )
}

function Card({
  title,
  icon,
  className = '',
  children,
}: {
  title: string
  icon?: React.ReactNode
  className?: string
  children: React.ReactNode
}) {
  return (
    <div className={`rounded-lg border border-border bg-card p-3 ${className}`}>
      <div className="flex items-center gap-1.5 text-[11px] uppercase tracking-wider font-semibold text-muted-foreground mb-1.5">
        {icon}
        {title}
      </div>
      {children}
    </div>
  )
}
