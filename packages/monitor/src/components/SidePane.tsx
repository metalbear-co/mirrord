import { Button, Card } from '@metalbear/ui'
import { FileJson, Plus, FlaskConical, RotateCcw } from 'lucide-react'
import type { SessionInfo } from '../types'
import type { UseChaosRules } from '../hooks/useChaosRules'
import type { UseReplay } from '../hooks/useReplay'
import type { UseQueueReplay } from '../hooks/useQueueReplay'
import { strings } from '../strings'
import ConfigTab from './ConfigTab'
import CopyButton from './CopyButton'
import ChaosPane, { type ChaosFormRequest } from './chaos/ChaosPane'
import ReplayPane from './replay/ReplayPane'

export type SidePaneTab = 'config' | 'chaos' | 'replay'

interface SidePaneProps {
  session: SessionInfo
  tab: SidePaneTab
  onTabChange: (tab: SidePaneTab) => void
  chaos: UseChaosRules
  replay: UseReplay
  queueReplay: UseQueueReplay | null
  seenHosts: string[]
  formRequest: ChaosFormRequest | null
  onNewRule: () => void
}

export default function SidePane({
  session,
  tab,
  onTabChange,
  chaos,
  replay,
  queueReplay,
  seenHosts,
  formRequest,
  onNewRule,
}: SidePaneProps) {
  const armed = chaos.armedCount > 0

  return (
    <Card className="flex h-full min-h-0 flex-col overflow-hidden p-0">
      <div
        className="surface-section border-border flex shrink-0 items-center border-b px-2"
        role="tablist"
      >
        <button
          type="button"
          role="tab"
          aria-selected={tab === 'config'}
          onClick={() => onTabChange('config')}
          className={
            'text-body flex items-center gap-1.5 border-b-2 px-3 pb-2 pt-2.5 transition-colors ' +
            (tab === 'config'
              ? 'border-primary text-foreground font-semibold'
              : 'text-muted-foreground hover:text-foreground border-transparent')
          }
        >
          <FileJson className="h-3 w-3" />
          {strings.session.configTab}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === 'chaos'}
          onClick={() => onTabChange('chaos')}
          className={
            'text-body flex items-center gap-1.5 border-b-2 px-3 pb-2 pt-2.5 transition-colors ' +
            (tab === 'chaos'
              ? armed
                ? 'border-chaos text-chaos font-semibold'
                : 'border-primary text-foreground font-semibold'
              : 'text-muted-foreground hover:text-foreground border-transparent')
          }
        >
          <FlaskConical className="h-3 w-3" />
          {strings.session.chaosTab}
          {armed && (
            <span
              className="bg-chaos/20 text-chaos rounded-full px-1.5 font-semibold"
              style={{ fontSize: 10 }}
            >
              {chaos.armedCount}
            </span>
          )}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === 'replay'}
          onClick={() => onTabChange('replay')}
          className={
            'text-body flex items-center gap-1.5 border-b-2 px-3 pb-2 pt-2.5 transition-colors ' +
            (tab === 'replay'
              ? 'border-primary text-foreground font-semibold'
              : 'text-muted-foreground hover:text-foreground border-transparent')
          }
        >
          <RotateCcw className="h-3 w-3" />
          {strings.session.replayTab}
        </button>

        <span className="ml-auto flex items-center pr-1">
          {tab === 'config' && (
            <CopyButton
              getText={() => JSON.stringify(session.config, null, 2)}
              title={strings.session.copyConfig}
            />
          )}
          {tab === 'chaos' && (
            <Button
              size="sm"
              variant="outline"
              className="h-6 px-2"
              onClick={onNewRule}
            >
              <Plus className="mr-1 h-3 w-3" />
              <span className="text-meta font-semibold">
                {strings.chaos.newRule}
              </span>
            </Button>
          )}
        </span>
      </div>

      {tab === 'config' && (
        <div className="min-h-0 flex-1 overflow-auto">
          <ConfigTab config={session.config} />
        </div>
      )}
      {tab === 'chaos' && (
        <ChaosPane
          chaos={chaos}
          seenHosts={seenHosts}
          formRequest={formRequest}
        />
      )}
      {tab === 'replay' && <ReplayPane replay={replay} queue={queueReplay} />}
    </Card>
  )
}
