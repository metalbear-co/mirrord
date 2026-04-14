import { Activity } from 'lucide-react'
import { strings } from '../strings'

export default function EmptySessionState() {
  return (
    <div className="flex flex-col items-center justify-center h-full text-muted-foreground gap-3">
      <Activity className="h-8 w-8 opacity-20" />
      <p className="text-sm font-medium">{strings.app.emptyTitle}</p>
      <p className="text-xs opacity-60 max-w-xs text-center">{strings.app.emptyBody}</p>
    </div>
  )
}
