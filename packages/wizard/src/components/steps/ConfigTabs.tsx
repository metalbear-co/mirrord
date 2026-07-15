import { useToast } from '../../hooks/use-toast'
import { useState, useEffect } from 'react'
import { Copy, Save, Download } from 'lucide-react'
import { Button, Label, Separator, Textarea } from '@metalbear/ui'
import { getConfigString, readCurrentTargetDetails, readIncoming } from '../JsonUtils'
import { useConfigData, DefaultConfig } from '../UserDataContext'
import type { ToggleableConfigFor_IncomingFileConfig } from '../../mirrord-schema'
import NetworkTab from './NetworkTab'
import TargetTab from './TargetTab'

type CurrentTabId = 'target' | 'network' | 'export'

interface ConfigTabsProps {
  currentTab: CurrentTabId
  onTabChange: (tab: CurrentTabId) => void
  onCanAdvanceChange: (canAdvance: boolean) => void
}

const ConfigTabs = ({ currentTab, onTabChange, onCanAdvanceChange }: ConfigTabsProps) => {
  const { config } = useConfigData()
  const [savedIncoming, setSavedIncoming] = useState<ToggleableConfigFor_IncomingFileConfig>(
    readIncoming(config),
  )
  const [portConflicts, setPortConflicts] = useState<boolean>(false)
  const [targetPorts, setTargetPorts] = useState<number[]>([])

  const { toast } = useToast()

  const targetNotSelected = (): boolean => {
    return typeof readCurrentTargetDetails(config).name !== 'string'
  }

  // Notify parent about whether we can advance
  useEffect(() => {
    const targetSelected = typeof readCurrentTargetDetails(config).name === 'string'
    const canAdvance = targetSelected && !portConflicts
    onCanAdvanceChange(canAdvance)
  }, [config, portConflicts, onCanAdvanceChange])

  const copyToClipboard = async () => {
    const jsonToCopy = getConfigString(config)
    await navigator.clipboard.writeText(jsonToCopy)
    toast({
      title: 'Copied to clipboard',
      description: 'Configuration JSON has been copied to your clipboard.',
    })
  }

  const downloadConfig = () => {
    const jsonContent = getConfigString(config)
    const blob = new Blob([jsonContent], { type: 'application/json' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = 'mirrord.json'
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    URL.revokeObjectURL(url)
    toast({
      title: 'Downloaded',
      description: 'Configuration saved as mirrord.json',
    })
  }

  return (
    <div className="space-y-6">
      {/* Tab Navigation */}
      <div className="border-border flex border-b">
        <button
          onClick={() => onTabChange('target')}
          className={`-mb-px border-b-2 px-4 py-2.5 text-sm font-medium transition-colors ${
            currentTab === 'target'
              ? 'border-primary text-primary'
              : 'text-muted-foreground hover:text-foreground border-transparent'
          }`}
        >
          Target
        </button>
        <button
          onClick={() => !targetNotSelected() && onTabChange('network')}
          className={`-mb-px border-b-2 px-4 py-2.5 text-sm font-medium transition-colors ${
            currentTab === 'network'
              ? 'border-primary text-primary'
              : 'text-muted-foreground hover:text-foreground border-transparent'
          } ${targetNotSelected() ? 'cursor-not-allowed opacity-50' : ''}`}
        >
          Network
        </button>
        <button
          onClick={() => !targetNotSelected() && !portConflicts && onTabChange('export')}
          className={`-mb-px border-b-2 px-4 py-2.5 text-sm font-medium transition-colors ${
            currentTab === 'export'
              ? 'border-primary text-primary'
              : 'text-muted-foreground hover:text-foreground border-transparent'
          } ${targetNotSelected() || portConflicts ? 'cursor-not-allowed opacity-50' : ''}`}
        >
          Export
        </button>
      </div>

      {/* Tab Content */}
      <div key={currentTab} className="animate-tab-in">
        {currentTab === 'target' && <TargetTab setTargetPorts={setTargetPorts} />}

        {currentTab === 'network' && (
          <NetworkTab
            savedIncoming={savedIncoming}
            targetPorts={targetPorts}
            setSavedIncoming={setSavedIncoming}
            setPortConflicts={setPortConflicts}
          />
        )}

        {currentTab === 'export' && (
          <div className="space-y-6">
            <div className="border-border flex items-center gap-3 border-b pb-4">
              <div className="bg-primary/10 flex h-10 w-10 items-center justify-center rounded-lg">
                <Save className="text-primary h-5 w-5" />
              </div>
              <div>
                <h3 className="text-lg font-semibold">Export Configuration</h3>
                <p className="text-muted-foreground text-sm">
                  Review and export your mirrord.json configuration
                </p>
              </div>
            </div>

            <div className="space-y-2">
              <Label htmlFor="json-editor">Configuration JSON</Label>
              <Textarea
                id="json-editor"
                className="font-code min-h-[200px] resize-none text-sm"
                value={getConfigString(config)}
                readOnly
                placeholder={getConfigString(DefaultConfig)}
              />
            </div>

            <div className="flex flex-wrap gap-2">
              <Button variant="outline" size="sm" onClick={() => void copyToClipboard()}>
                <Copy className="mr-2 h-4 w-4" />
                Copy to Clipboard
              </Button>
              <Button variant="outline" size="sm" onClick={downloadConfig}>
                <Download className="mr-2 h-4 w-4" />
                Download File
              </Button>
            </div>

            <Separator />

            <div className="space-y-3">
              <h4 className="text-foreground font-medium">How to use your configuration:</h4>
              <div className="text-muted-foreground space-y-2 text-sm">
                <p>
                  <strong>CLI:</strong> Use the{' '}
                  <code className="bg-muted rounded px-1 py-0.5 text-xs">
                    -f &lt;CONFIG_PATH&gt;
                  </code>{' '}
                  flag
                </p>
                <p>
                  <strong>VSCode / JetBrains:</strong> Create a{' '}
                  <code className="bg-muted rounded px-1 py-0.5 text-xs">
                    .mirrord/mirrord.json
                  </code>{' '}
                  file
                </p>
                <p>
                  See the{' '}
                  <a
                    href="https://mirrord.dev/docs/reference/configuration/"
                    target="_blank"
                    rel="noopener noreferrer"
                    className="text-primary hover:underline"
                  >
                    configuration documentation
                  </a>{' '}
                  for more details.
                </p>
              </div>
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

export default ConfigTabs
