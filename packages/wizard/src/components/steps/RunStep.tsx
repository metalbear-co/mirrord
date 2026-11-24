import { Terminal } from 'lucide-react'
import { Input, Label } from '@metalbear/ui'
import { useDraft } from '../DraftContext'
import { commandError } from '../../lib/up'
import { strings } from '../../strings'

/** The run questions of `mirrord up init`. The run type is not asked: `exec` fits most services. */
const RunStep = () => {
  const { draft, setDraft } = useDraft()
  const problem = commandError(draft.command)

  return (
    <div className="space-y-6">
      <div className="border-border flex items-center gap-3 border-b pb-4">
        <div className="bg-primary/10 flex h-10 w-10 items-center justify-center rounded-lg">
          <Terminal className="text-primary h-5 w-5" />
        </div>
        <div>
          <h3 className="text-lg font-semibold">{strings.runStep.title}</h3>
          <p className="text-muted-foreground text-sm">
            {strings.runStep.subtitle}
          </p>
        </div>
      </div>

      <div className="space-y-2">
        <Label htmlFor="command" className="text-sm font-medium">
          {strings.runStep.command}
        </Label>
        <Input
          id="command"
          className="font-mono"
          value={draft.command}
          onChange={(e) => setDraft({ ...draft, command: e.target.value })}
        />
        {problem && draft.command !== '' && (
          <p className="text-destructive text-sm">{problem}</p>
        )}
      </div>

      <div className="space-y-2">
        <Label htmlFor="directory" className="text-sm font-medium">
          {strings.runStep.directory}
        </Label>
        <Input
          id="directory"
          className="font-mono"
          value={draft.directory}
          onChange={(e) => setDraft({ ...draft, directory: e.target.value })}
        />
        <p className="text-muted-foreground text-xs">
          {strings.runStep.directoryHelp}
        </p>
      </div>
    </div>
  )
}

export default RunStep
