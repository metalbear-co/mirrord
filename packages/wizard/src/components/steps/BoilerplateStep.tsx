import { Copy, Filter, Repeat, Check } from 'lucide-react'
import { Badge } from '@metalbear/ui'
import { useConfigData } from '../UserDataContext'
import {
  readBoilerplateType,
  updateConfigMode,
  updateConfigCopyTarget,
} from '../JsonUtils'
import { strings } from '../../strings'

type BoilerplateType = 'mirror' | 'steal' | 'replace'

interface ModeOption {
  id: BoilerplateType
  icon: React.ReactNode
  title: string
  description: string
  features: string[]
  recommended?: boolean
}

const modeOptions: ModeOption[] = [
  {
    id: 'mirror',
    icon: <Copy className="h-5 w-5" />,
    title: 'Mirror Mode',
    description:
      'Copy incoming traffic to your local environment without affecting the remote service. Perfect for debugging production issues safely.',
    features: ['Non-disruptive', 'Safe for production'],
    recommended: true,
  },
  {
    id: 'steal',
    icon: <Filter className="h-5 w-5" />,
    title: 'Filtering Mode',
    description:
      'Selectively intercept traffic based on HTTP headers or paths. Route specific requests to your local environment while others go to remote.',
    features: ['Selective traffic', 'Header-based routing'],
  },
  {
    id: 'replace',
    icon: <Repeat className="h-5 w-5" />,
    title: 'Replace Mode',
    description:
      'Completely replace the remote service with your local environment. All traffic is routed to your local process.',
    features: ['Full replacement', 'Scale down remote'],
  },
]

const BoilerplateStep = () => {
  const { config, setConfig } = useConfigData()
  const selectedMode = readBoilerplateType(config)

  const handleModeSelect = (mode: BoilerplateType) => {
    let newConfig = config

    switch (mode) {
      case 'mirror':
        newConfig = updateConfigMode('mirror', config)
        newConfig = updateConfigCopyTarget(false, false, newConfig)
        break
      case 'steal':
        newConfig = updateConfigMode('steal', config)
        newConfig = updateConfigCopyTarget(false, false, newConfig)
        break
      case 'replace':
        newConfig = updateConfigMode('steal', config)
        newConfig = updateConfigCopyTarget(true, true, newConfig)
        break
    }

    setConfig(newConfig)
  }

  return (
    <div className="space-y-6">
      <div className="mb-8 text-center">
        <h3 className="text-foreground mb-2 text-lg font-medium">
          {strings.boilerplateStep.heading}
        </h3>
        <p className="text-muted-foreground text-sm">
          {strings.boilerplateStep.subheading}
        </p>
      </div>

      <div className="space-y-3">
        {modeOptions.map((option) => {
          const isSelected = selectedMode === option.id

          return (
            <button
              key={option.id}
              onClick={() => handleModeSelect(option.id)}
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
                  {option.icon}
                </div>
                <div className="min-w-0 flex-grow">
                  <div className="mb-2 flex flex-wrap items-center gap-2">
                    <span
                      className={`font-semibold ${isSelected ? 'text-primary' : 'text-foreground'}`}
                    >
                      {option.title}
                    </span>
                    {option.recommended && (
                      <Badge
                        variant="outline"
                        className="bg-muted text-muted-foreground border-border text-xs"
                      >
                        {strings.boilerplateStep.recommended}
                      </Badge>
                    )}
                  </div>
                  <p className="text-muted-foreground mb-3 text-sm leading-relaxed">
                    {option.description}
                  </p>
                  <div className="flex flex-wrap gap-2">
                    {option.features.map((feature) => (
                      <Badge
                        key={feature}
                        variant="outline"
                        className={`text-xs font-normal ${isSelected ? 'border-primary/30 text-primary' : ''}`}
                      >
                        {feature}
                      </Badge>
                    ))}
                  </div>
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

export default BoilerplateStep
