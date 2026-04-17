import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogHeader,
  DialogTitle,
  Label,
  Switch,
} from '@metalbear/ui'

interface Props {
  open: boolean
  onOpenChange: (open: boolean) => void
  telemetryEnabled: boolean
  onTelemetryChange: (enabled: boolean) => void
}

export default function SettingsDialog({
  open,
  onOpenChange,
  telemetryEnabled,
  onTelemetryChange,
}: Props) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="sm:max-w-md">
        <DialogHeader>
          <DialogTitle>Settings</DialogTitle>
          <DialogDescription>
            Preferences for this browser. Stored locally and never sent anywhere.
          </DialogDescription>
        </DialogHeader>

        <div className="mt-2 space-y-6">
          <div className="flex items-start justify-between gap-6">
            <div className="space-y-1.5">
              <Label htmlFor="telemetry-toggle" className="text-body font-medium">
                Share anonymous usage analytics
              </Label>
              <p className="text-body-sm text-foreground/60 leading-relaxed">
                Helps us see which parts of the session monitor are used. File paths, pod
                names, URLs, and request bodies are never captured — session replays mask
                all text and input values, so only click and scroll behavior is recorded.
              </p>
            </div>
            <Switch
              id="telemetry-toggle"
              checked={telemetryEnabled}
              onCheckedChange={onTelemetryChange}
              className="mt-1 shrink-0"
            />
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}
