import { Copy, Download, Plus, Save, X, AlertCircle } from 'lucide-react'
import { Badge, Button, Label, Separator, Textarea } from '@metalbear/ui'
import { useToast } from '../../hooks/use-toast'
import { useDraft } from '../DraftContext'
import { useRenderedYaml } from '../../lib/queries'
import { strings } from '../../strings'

interface ExportStepProps {
  /** Like answering yes to "Add another service?". */
  onAddService: () => void
}

/** The services so far and their rendered `mirrord-up.yaml`. */
const ExportStep = ({ onAddService }: ExportStepProps) => {
  const { services, setServices } = useDraft()
  const rendered = useRenderedYaml(services)
  const yaml = rendered.data ?? ''
  const { toast } = useToast()

  const removeService = (index: number) => {
    setServices(services.filter((_, i) => i !== index))
  }

  const copyToClipboard = async () => {
    await navigator.clipboard.writeText(yaml)
    toast({
      title: 'Copied to clipboard',
      description: 'mirrord-up.yaml has been copied to your clipboard.',
    })
  }

  const downloadConfig = () => {
    const blob = new Blob([yaml], { type: 'application/yaml' })
    const url = URL.createObjectURL(blob)
    const a = document.createElement('a')
    a.href = url
    a.download = strings.exportStep.fileName
    document.body.appendChild(a)
    a.click()
    document.body.removeChild(a)
    URL.revokeObjectURL(url)
    toast({
      title: 'Downloaded',
      description: `Configuration saved as ${strings.exportStep.fileName}`,
    })
  }

  return (
    <div className="space-y-6">
      <div className="border-border flex items-center gap-3 border-b pb-4">
        <div className="bg-primary/10 flex h-10 w-10 items-center justify-center rounded-lg">
          <Save className="text-primary h-5 w-5" />
        </div>
        <div>
          <h3 className="text-lg font-semibold">{strings.exportStep.title}</h3>
          <p className="text-muted-foreground text-sm">
            {strings.exportStep.subtitle}
          </p>
        </div>
      </div>

      <div className="space-y-2">
        <Label>{strings.exportStep.services}</Label>
        <ul className="border-border divide-border divide-y rounded-lg border">
          {services.map((service, index) => (
            <li
              key={service.name}
              className="flex items-center justify-between gap-3 px-3 py-2"
            >
              <span className="flex min-w-0 items-center gap-2">
                <span className="font-medium">{service.name}</span>
                <Badge variant="outline" className="text-xs">
                  {service.mode}
                </Badge>
                <span className="text-muted-foreground truncate font-mono text-xs">
                  {service.target?.path ?? strings.exportStep.noTarget}
                </span>
              </span>
              <button
                aria-label={`${strings.exportStep.removeService} ${service.name}`}
                className="hover:bg-muted rounded-full p-1"
                onClick={() => removeService(index)}
              >
                <X className="text-muted-foreground h-4 w-4" />
              </button>
            </li>
          ))}
        </ul>
        <Button variant="outline" size="sm" onClick={onAddService}>
          <Plus className="mr-2 h-4 w-4" />
          {strings.exportStep.addAnother}
        </Button>
      </div>

      {services.length > 0 && (
        <>
          <div className="space-y-2">
            <Label htmlFor="yaml-preview">{strings.exportStep.yaml}</Label>
            {rendered.isError ? (
              <p className="text-destructive flex items-center gap-2 text-sm">
                <AlertCircle className="h-4 w-4 flex-shrink-0" />
                {rendered.error.message}
              </p>
            ) : (
              <Textarea
                id="yaml-preview"
                className="font-code min-h-[200px] resize-none text-sm"
                value={rendered.isLoading ? strings.exportStep.rendering : yaml}
                readOnly
              />
            )}
          </div>

          <div className="flex flex-wrap gap-2">
            <Button
              variant="outline"
              size="sm"
              disabled={yaml === ''}
              onClick={() => void copyToClipboard()}
            >
              <Copy className="mr-2 h-4 w-4" />
              {strings.exportStep.copyToClipboard}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={yaml === ''}
              onClick={downloadConfig}
            >
              <Download className="mr-2 h-4 w-4" />
              {strings.exportStep.downloadFile}
            </Button>
          </div>
        </>
      )}

      <Separator />

      <div className="space-y-3">
        <h4 className="text-foreground font-medium">
          {strings.exportStep.howToUse}
        </h4>
        <div className="text-muted-foreground space-y-2 text-sm">
          <p>
            <strong>{strings.exportStep.cliLabel}</strong>{' '}
            {strings.exportStep.cliRun}{' '}
            <InlineCode>{strings.exportStep.cliCommand}</InlineCode>
          </p>
          <p>
            {strings.exportStep.seeThe}{' '}
            <a
              href={strings.exportStep.docsUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="text-primary hover:underline"
            >
              {strings.exportStep.docs}
            </a>{' '}
            {strings.exportStep.forMoreDetails}
          </p>
          <p>
            <strong>{strings.exportStep.commonLabel}</strong>{' '}
            {strings.exportStep.commonPrefix}{' '}
            <InlineCode>{strings.exportStep.commonOperator}</InlineCode>{' '}
            {strings.exportStep.commonAnd}{' '}
            <InlineCode>{strings.exportStep.commonTelemetry}</InlineCode>
            {strings.exportStep.commonGoUnder}{' '}
            <InlineCode>{strings.exportStep.commonBlock}</InlineCode>
            {strings.exportStep.period}{' '}
            <a
              href={strings.exportStep.commonDocsUrl}
              target="_blank"
              rel="noopener noreferrer"
              className="text-primary hover:underline"
            >
              {strings.exportStep.commonDocs}
            </a>
            {strings.exportStep.period}
          </p>
        </div>
      </div>
    </div>
  )
}

const InlineCode = ({ children }: { children: React.ReactNode }) => (
  <code className="bg-muted rounded px-1 py-0.5 text-xs">{children}</code>
)

export default ExportStep
