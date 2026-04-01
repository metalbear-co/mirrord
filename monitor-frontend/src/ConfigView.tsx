import { Settings } from 'lucide-react'
import type { SessionInfo } from './types'

interface ConfigViewProps {
  session: SessionInfo | undefined
}

export default function ConfigView({ session }: ConfigViewProps) {
  if (!session) {
    return (
      <div className="flex flex-col items-center justify-center h-full text-muted-foreground gap-2">
        <Settings className="h-8 w-8 opacity-30" />
        <p>Select a session to view its configuration</p>
      </div>
    )
  }

  return (
    <div className="h-full overflow-auto p-4">
      <div className="rounded-lg border border-border bg-card/50 overflow-hidden">
        <div className="px-4 py-2 border-b border-border bg-card/30 text-xs text-muted-foreground font-medium">
          Configuration for {session.target}
        </div>
        <pre className="p-4 text-xs font-mono text-foreground/90 whitespace-pre-wrap overflow-auto">
          {JSON.stringify(session.config, null, 2)}
        </pre>
      </div>
    </div>
  )
}
