import { Copy, Split, Repeat, Check, AlertCircle } from 'lucide-react'
import { Badge } from '@metalbear/ui'
import { useDraft } from '../DraftContext'
import { useModes } from '../../lib/queries'
import type { ModeId } from '../../lib/up'
import { strings } from '../../strings'

const modeIcons: Record<ModeId, React.ReactNode> = {
  split: <Split className="h-5 w-5" />,
  mirror: <Copy className="h-5 w-5" />,
  replace: <Repeat className="h-5 w-5" />,
}

/** The mode question of `mirrord up init`. */
const ModeStep = () => {
  const { draft, setDraft } = useDraft()
  const modes = useModes()

  // The target may not be allowed in the new mode, so it is picked again.
  const selectMode = (mode: ModeId) => {
    if (mode === draft.mode) return
    setDraft({
      ...draft,
      mode,
      target: null,
      targetless: false,
      name: '',
      unselectedPorts: [],
    })
  }

  return (
    <div className="space-y-6">
      <div className="mb-8 text-center">
        <h3 className="text-foreground mb-2 text-lg font-medium">
          {strings.modeStep.heading}
        </h3>
      </div>

      {modes.isLoading && (
        <p className="text-muted-foreground text-center text-sm">
          {strings.modeStep.loading}
        </p>
      )}
      {modes.isError && (
        <p className="text-destructive flex items-center justify-center gap-2 text-sm">
          <AlertCircle className="h-4 w-4" />
          {strings.modeStep.failed}
        </p>
      )}

      <div className="space-y-3">
        {(modes.data ?? []).map((mode) => {
          const isSelected = draft.mode === mode.id

          return (
            <button
              key={mode.id}
              onClick={() => selectMode(mode.id)}
              className={`w-full rounded-xl border p-5 text-left transition-all duration-200 ${
                isSelected
                  ? 'border-primary bg-primary/5 shadow-brand ring-primary/20 ring-1'
                  : 'border-border hover:border-primary/40 hover:bg-primary/5 hover:shadow-md active:scale-[0.99]'
              } `}
            >
              <div className="flex items-start gap-4">
                <div
                  className={`flex h-12 w-12 flex-shrink-0 items-center justify-center rounded-xl transition-colors duration-200 ${
                    isSelected
                      ? 'bg-primary shadow-brand text-white'
                      : 'bg-muted text-muted-foreground'
                  } `}
                >
                  {modeIcons[mode.id]}
                </div>
                <div className="min-w-0 flex-grow">
                  <div className="mb-2 flex flex-wrap items-center gap-2">
                    <span
                      className={`font-semibold ${isSelected ? 'text-primary' : 'text-foreground'}`}
                    >
                      {mode.label}
                    </span>
                    {mode.recommended && (
                      <Badge
                        variant="outline"
                        className="bg-muted text-muted-foreground border-border text-xs"
                      >
                        {strings.modeStep.recommended}
                      </Badge>
                    )}
                  </div>
                  <p className="text-muted-foreground text-sm leading-relaxed">
                    {mode.description}
                  </p>
                </div>
                <div
                  className={`flex-shrink-0 transition-all duration-200 ${isSelected ? 'scale-100 opacity-100' : 'scale-75 opacity-0'}`}
                >
                  <div className="bg-primary flex h-6 w-6 items-center justify-center rounded-full">
                    <Check className="h-4 w-4 text-white" />
                  </div>
                </div>
              </div>
            </button>
          )
        })}
      </div>
    </div>
  )
}

export default ModeStep
