import { Trash2 } from 'lucide-react'
import { Button, Input } from '@metalbear/ui'
import { removeSingleFilter } from '../JsonUtils'
import { useConfigData } from '../UserDataContext'

export const HttpFilter = ({
  initValue,
  inputType,
}: {
  initValue: string
  inputType: 'header' | 'path'
}) => {
  const { config, setConfig } = useConfigData()

  return (
    <div className="space-y-2">
      <div className="flex items-center gap-2 text-muted-foreground">
        {'/'}
        <Input
          placeholder="e.g., x-mirrord-test: true"
          value={initValue}
          readOnly={true}
          className="flex-1 text-foreground"
        />
        {'/'}

        <Button
          type="button"
          variant="outline"
          size="sm"
          onClick={() => {
            const newConfig = removeSingleFilter(
              { value: initValue, type: inputType },
              config,
            )
            setConfig(newConfig)
          }}
        >
          <Trash2 className="h-4 w-4" />
        </Button>
      </div>
    </div>
  )
}

export default HttpFilter
