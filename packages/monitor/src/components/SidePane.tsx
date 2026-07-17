import { Button, Card, cn } from '@metalbear/ui'
import { FileJson, Plus, Zap } from 'lucide-react'
import type { SessionInfo } from '../types'
import type { UseChaosRules } from '../hooks/useChaosRules'
import { strings } from '../strings'
import ConfigTab from './ConfigTab'
import CopyButton from './CopyButton'
import ChaosPane, { type ChaosFormRequest } from './chaos/ChaosPane'

export type SidePaneTab = 'config' | 'chaos'

interface SidePaneProps {
  session: SessionInfo
  tab: SidePaneTab
  onTabChange: (tab: SidePaneTab) => void
  chaos: UseChaosRules
  seenHosts: string[]
  formRequest: ChaosFormRequest | null
  onNewRule: () => void
}

export default function SidePane({
  session,
  tab,
  onTabChange,
  chaos,
  seenHosts,
  formRequest,
  onNewRule,
}: SidePaneProps) {
  const armed = chaos.armedCount > 0

  return (
    <Card
      className={cn(
        'flex h-full min-h-0 flex-col overflow-hidden p-0 transition-colors duration-300',
        armed && 'border-chaos',
      )}
    >
      <div
        className="surface-section border-border flex shrink-0 items-center border-b px-2"
        role="tablist"
      >
        <button
          type="button"
          role="tab"
          aria-selected={tab === 'config'}
          onClick={() => onTabChange('config')}
          className={cn(
            'text-body flex items-center gap-1.5 border-b-2 px-3 pb-2 pt-2.5 transition-colors',
            tab === 'config'
              ? 'border-primary text-foreground font-semibold'
              : 'text-muted-foreground hover:text-foreground border-transparent',
          )}
        >
          <FileJson className="h-3 w-3" />
          {strings.session.configTab}
        </button>
        <button
          type="button"
          role="tab"
          aria-selected={tab === 'chaos'}
          onClick={() => onTabChange('chaos')}
          className={cn(
            'text-body flex items-center gap-1.5 border-b-2 px-3 pb-2 pt-2.5 transition-colors',
            tab === 'chaos'
              ? cn(
                  'font-semibold',
                  armed
                    ? 'border-chaos text-chaos'
                    : 'border-primary text-foreground',
                )
              : 'text-muted-foreground hover:text-foreground border-transparent',
          )}
        >
          <Zap className="h-3 w-3" />
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

        <span className="ml-auto flex items-center pr-1">
          {tab === 'config' ? (
            <CopyButton
              getText={() => JSON.stringify(session.config, null, 2)}
              title={strings.session.copyConfig}
            />
          ) : (
            <Button size="sm" className="h-6 px-2" onClick={onNewRule}>
              <Plus className="mr-1 h-3 w-3" />
              {strings.chaos.newRule}
            </Button>
          )}
        </span>
      </div>

      {tab === 'config' ? (
        <div className="min-h-0 flex-1 overflow-auto">
          <ConfigTab config={session.config} />
        </div>
      ) : (
        <ChaosPane
          chaos={chaos}
          seenHosts={seenHosts}
          formRequest={formRequest}
        />
      )}
    </Card>
  )
}
