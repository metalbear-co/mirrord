import { Network } from 'lucide-react'
import { Checkbox, Input, Label, Switch } from '@metalbear/ui'
import { useDraft } from '../DraftContext'
import { useModes, usePorts } from '../../lib/queries'
import { trafficError } from '../../lib/up'
import { strings } from '../../strings'

/** The traffic questions of `mirrord up init`, asked only when the user wants to set them. */
const TrafficStep = () => {
  const { draft, setDraft } = useDraft()
  const modes = useModes()
  const mode = modes.data?.find((m) => m.id === draft.mode)
  const portsError = trafficError(draft)
  // Like `mirrord up init`, undetectable ports offer none to intercept.
  const detected = usePorts(draft.target)
  const detectedPorts = detected.data ?? []

  const toggleIntercepted = (port: number, intercepted: boolean) => {
    const unselectedPorts = intercepted
      ? draft.unselectedPorts.filter((p) => p !== port)
      : [...draft.unselectedPorts, port].sort((a, b) => a - b)
    setDraft({ ...draft, unselectedPorts })
  }

  return (
    <div className="space-y-6">
      <div className="border-border flex items-center gap-3 border-b pb-4">
        <div className="bg-primary/10 flex h-10 w-10 items-center justify-center rounded-lg">
          <Network className="text-primary h-5 w-5" />
        </div>
        <div>
          <h3 className="text-lg font-semibold">{strings.trafficStep.title}</h3>
          <p className="text-muted-foreground text-sm">
            {strings.trafficStep.subtitle}
          </p>
        </div>
      </div>

      <div className="flex items-center justify-between gap-4">
        <Label htmlFor="configure-traffic" className="text-sm font-medium">
          {mode?.traffic_question}
        </Label>
        <Switch
          id="configure-traffic"
          checked={draft.configureTraffic}
          onCheckedChange={(configureTraffic: boolean) =>
            setDraft({ ...draft, configureTraffic })
          }
        />
      </div>

      {draft.configureTraffic && (
        <div className="space-y-6">
          {mode?.http_filter && (
            <div className="space-y-2">
              <Label htmlFor="header-filter" className="text-sm font-medium">
                {strings.trafficStep.headerFilter}
              </Label>
              <Input
                id="header-filter"
                value={draft.headerFilter}
                onChange={(e) =>
                  setDraft({ ...draft, headerFilter: e.target.value })
                }
              />
              <p className="text-muted-foreground text-xs">
                {strings.trafficStep.headerFilterHelp}
              </p>
            </div>
          )}

          {detected.isLoading ? (
            <p className="text-muted-foreground text-sm">
              {strings.trafficStep.detectingPorts}
            </p>
          ) : detected.isError ? (
            <p className="text-muted-foreground text-sm">
              {detected.error.message}
            </p>
          ) : detectedPorts.length === 0 ? (
            <p className="text-muted-foreground text-sm">
              {strings.trafficStep.noPortsDetected}
            </p>
          ) : (
            <div className="space-y-2">
              <Label className="text-sm font-medium">
                {strings.trafficStep.interceptPorts}
              </Label>
              <div className="flex flex-wrap gap-4">
                {detectedPorts.map((port) => (
                  <label
                    key={port}
                    className="flex items-center gap-2 font-mono text-sm"
                  >
                    <Checkbox
                      checked={!draft.unselectedPorts.includes(port)}
                      onCheckedChange={(checked) =>
                        toggleIntercepted(port, checked === true)
                      }
                      aria-label={`intercept port ${port}`}
                    />
                    {port}
                  </label>
                ))}
              </div>
              <p className="text-muted-foreground text-xs">
                {strings.trafficStep.interceptPortsHelp}
              </p>
            </div>
          )}

          <div className="space-y-2">
            <Label htmlFor="ignore-ports" className="text-sm font-medium">
              {!detected.isLoading && detectedPorts.length === 0
                ? strings.trafficStep.ignorePorts
                : strings.trafficStep.otherIgnorePorts}
            </Label>
            <Input
              id="ignore-ports"
              value={draft.otherIgnoredPorts}
              onChange={(e) =>
                setDraft({ ...draft, otherIgnoredPorts: e.target.value })
              }
            />
            {portsError && (
              <p className="text-destructive text-sm">{portsError}</p>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

export default TrafficStep
