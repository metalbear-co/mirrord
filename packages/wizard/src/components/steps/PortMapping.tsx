import { ArrowRight, Trash2 } from 'lucide-react'
import { Button, Input } from '@metalbear/ui'
import {
  addRemoveOrUpdateMapping,
  getLocalPort,
  readBoilerplateType,
  readCurrentPorts,
  removePortandMapping,
} from '../JsonUtils'
import { useState } from 'react'
import { useConfigData } from '../UserDataContext'
import { useToast } from '../../hooks/use-toast'

const PortMappingEntry = ({
  remotePort,
  detectedPort,
  setPortConflicts,
}: {
  remotePort: number
  detectedPort: boolean
  setPortConflicts: (value: boolean) => void
}) => {
  const { config, setConfig } = useConfigData()
  const [inputContents, setInputContents] = useState<number>(
    getLocalPort(remotePort, config),
  )
  const [outlineConflict, setOutlineConflict] = useState<boolean>(false)

  const { toast, dismiss } = useToast()
  const localPortConflict = () => {
    setOutlineConflict(true)
    setPortConflicts(true)
    toast({
      title: 'Local Port Conflict!',
      description:
        'Multiple port mappings have the same local port. Local ports should be unique.',
      duration: 31536000000,
    })
  }
  const resolveConflict = () => {
    setOutlineConflict(false)
    setPortConflicts(false)
    dismiss()
  }

  return (
    <div className="border-border space-y-3 rounded-lg border p-3">
      <div className="flex items-center gap-3">
        <div className="flex-1 font-mono">
          <p className="border-border bg-background ring-offset-background flex h-10 w-full rounded-md border px-3 py-2 text-base md:text-sm">
            {remotePort}
          </p>
        </div>

        <ArrowRight className="text-muted-foreground h-4 w-4 flex-shrink-0" />

        <div className="flex-1">
          <Input
            type="text"
            pattern="[0-9]*"
            className={
              outlineConflict
                ? 'font-mono ring-2 ring-red-500 ring-offset-2 focus-visible:ring-red-500'
                : 'font-mono'
            }
            value={inputContents}
            onChange={(event) => {
              const newValue = +event.target.value.replace(/\s/g, '')
              if (!isNaN(newValue)) {
                if (
                  readCurrentPorts(config).filter(
                    (remote) => getLocalPort(remote, config) === newValue,
                  ).length > 0
                ) {
                  localPortConflict()
                } else {
                  resolveConflict()
                }
                const newConfig = addRemoveOrUpdateMapping(
                  remotePort,
                  newValue,
                  config,
                )
                setConfig(newConfig)

                setInputContents(newValue)
              }
            }}
          />
        </div>

        {!(readBoilerplateType(config) === 'replace' && detectedPort) && (
          <Button
            variant="ghost"
            size="sm"
            className="h-8 w-8 p-0 text-red-500 hover:bg-red-50 hover:text-red-600"
            onClick={() => {
              const newConfig = removePortandMapping(remotePort, config)
              setConfig(newConfig)
              resolveConflict()
            }}
          >
            <Trash2 className="h-4 w-4" />
          </Button>
        )}
      </div>
    </div>
  )
}

export default PortMappingEntry
