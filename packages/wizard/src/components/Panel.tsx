import { Button } from '@metalbear/ui'

interface PanelProps {
  icon: React.ReactNode
  title: string
  description: string
  buttonText: string
  onClick: () => void
  primary?: boolean
}

const Panel = ({
  icon,
  title,
  description,
  buttonText,
  onClick,
  primary = false,
}: PanelProps) => {
  return (
    <div
      className={`card card-hover flex h-full flex-col p-6 ${primary ? 'border-primary/30' : ''} `}
    >
      <div
        className={`mb-4 flex h-12 w-12 items-center justify-center rounded-lg ${primary ? 'bg-primary/10 text-primary' : 'bg-muted text-muted-foreground'} `}
      >
        {icon}
      </div>
      <h3 className="text-foreground mb-2 text-lg font-semibold">{title}</h3>
      <p className="text-muted-foreground mb-6 flex-grow text-sm">
        {description}
      </p>
      <Button
        onClick={onClick}
        variant={primary ? 'default' : 'outline'}
        className="w-full"
      >
        {buttonText}
      </Button>
    </div>
  )
}

export default Panel
