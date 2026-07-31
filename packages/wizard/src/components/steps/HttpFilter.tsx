import { Trash2 } from 'lucide-react'
import { Button, Input } from '@metalbear/ui'
import { removeSingleFilter } from '../JsonUtils'
import { useConfigData } from '../UserDataContext'

const HttpFilter = ({
  initValue,
  inputType,
}: {
  initValue: string
  inputType: 'header' | 'path'
}) => {
  const { config, setConfig } = useConfigData()

  return (
    <div className="space-y-2">
      <div className="text-muted-foreground flex items-center gap-2">
        {'/'}
        <Input
          placeholder="e.g., x-mirrord-test: true"
          value={initValue}
          readOnly={true}
          className="text-foreground flex-1"
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
