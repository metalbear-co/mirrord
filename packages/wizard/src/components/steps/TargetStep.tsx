import { useState } from 'react'
import { Server, Check, Search, Unplug, AlertCircle } from 'lucide-react'
import {
  Badge,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@metalbear/ui'
import { useDraft } from '../DraftContext'
import {
  useCurrentContext,
  useModes,
  useNamespaces,
  useWorkloads,
} from '../../lib/queries'
import {
  inNamespace,
  nameError,
  serviceNameFor,
  withoutKind,
  workloadKind,
  workloadName,
} from '../../lib/up'
import { strings } from '../../strings'

/** The target question of `mirrord up init`, listing what it lists. */
const TargetStep = () => {
  const { draft, setDraft, services } = useDraft()
  const modes = useModes()
  const mode = modes.data?.find((m) => m.id === draft.mode)
  const takenNames = services.map((service) => service.name)

  const currentContext = useCurrentContext()
  const namespaces = useNamespaces()
  const [pickedNamespace, setPickedNamespace] = useState<string | undefined>(
    draft.target?.namespace,
  )
  // Start on the context's namespace, like `mirrord up init`. List nothing until it is known, so
  // no workload is picked in another namespace first.
  const namespace = currentContext.isPending
    ? undefined
    : (pickedNamespace ?? currentContext.data?.namespace ?? 'default')
  const [kindFilter, setKindFilter] = useState<string>('all')
  const [search, setSearch] = useState('')

  const workloads = useWorkloads(namespace, draft.mode)
  const shownWorkloads = (workloads.data ?? []).filter(
    (path) =>
      (kindFilter === 'all' || workloadKind(path) === kindFilter) &&
      path.toLowerCase().includes(search.toLowerCase()),
  )
  // Without permission to list namespaces, offer the context's, like `mirrord up init`.
  const namespaceOptions =
    namespaces.data && namespaces.data.length > 0
      ? namespaces.data
      : namespace !== undefined
        ? [namespace]
        : []

  const changeNamespace = (picked: string) => {
    setPickedNamespace(picked)
    setDraft(inNamespace(draft, picked))
  }

  const selectWorkload = (path: string) => {
    if (namespace === undefined) return
    // Picking it again would reset the answers that depend on it.
    if (draft.target?.path === path && draft.target.namespace === namespace) {
      return
    }
    setDraft({
      ...draft,
      target: { path, namespace },
      targetless: false,
      name: serviceNameFor(workloadName(path), takenNames),
      unselectedPorts: [],
    })
  }

  const selectTargetless = () => {
    if (draft.targetless) return
    setDraft({
      ...draft,
      target: null,
      targetless: true,
      name: '',
      unselectedPorts: [],
    })
  }

  const nameProblem = draft.targetless
    ? nameError(draft.name, takenNames)
    : null
  const clusterError =
    currentContext.error ?? namespaces.error ?? workloads.error

  return (
    <div className="space-y-6">
      <div className="border-border flex items-center gap-3 border-b pb-4">
        <div className="bg-primary/10 flex h-10 w-10 items-center justify-center rounded-lg">
          <Server className="text-primary h-5 w-5" />
        </div>
        <div>
          <h3 className="text-lg font-semibold">{strings.targetStep.title}</h3>
          <p className="text-muted-foreground text-sm">
            {strings.targetStep.subtitle}
          </p>
        </div>
      </div>

      <div className="space-y-5">
        <div className="space-y-2">
          <Label className="text-sm font-medium">
            {strings.targetStep.kubeContext}
          </Label>
          <p className="bg-muted text-foreground rounded-md px-3 py-2 font-mono text-sm">
            {currentContext.data?.name ?? strings.targetStep.unknownContext}
          </p>
        </div>

        {clusterError && (
          <p className="text-destructive bg-destructive/10 flex items-center gap-2 rounded-lg p-3 text-sm">
            <AlertCircle className="h-4 w-4 flex-shrink-0" />
            {clusterError.message}
          </p>
        )}

        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label className="text-sm font-medium">
              {strings.targetStep.namespace}
            </Label>
            <Select
              {...(namespace !== undefined ? { value: namespace } : {})}
              onValueChange={changeNamespace}
            >
              <SelectTrigger className="h-10" aria-label="namespace">
                <SelectValue />
              </SelectTrigger>
              <SelectContent className="bg-card border-border border">
                {namespaceOptions.length === 0 ? (
                  <SelectItem value="none" disabled>
                    {strings.targetStep.noNamespaces}
                  </SelectItem>
                ) : (
                  namespaceOptions.map((ns) => (
                    <SelectItem key={ns} value={ns}>
                      {ns}
                    </SelectItem>
                  ))
                )}
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-2">
            <Label className="text-sm font-medium">
              {strings.targetStep.resourceType}
            </Label>
            <Select value={kindFilter} onValueChange={setKindFilter}>
              <SelectTrigger className="h-10" aria-label="resource type">
                <SelectValue />
              </SelectTrigger>
              <SelectContent className="bg-card border-border border">
                <SelectItem value="all">
                  {strings.targetStep.allTypes}
                </SelectItem>
                {(mode?.target_types ?? []).map((kind) => (
                  <SelectItem key={kind} value={kind}>
                    {kind}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>

        <div className="space-y-2">
          <Label className="text-sm font-medium">
            {strings.targetStep.workload}
          </Label>
          <div className="border-border overflow-hidden rounded-xl border">
            <div className="border-border bg-muted/30 border-b p-3">
              <div className="relative">
                <Search className="text-muted-foreground absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2" />
                <Input
                  placeholder={strings.targetStep.searchTargets}
                  className="bg-card h-9 pl-9"
                  value={search}
                  onChange={(e) => setSearch(e.target.value)}
                />
              </div>
            </div>
            <div className="max-h-60 overflow-y-auto p-2">
              {namespace === undefined || workloads.isLoading ? (
                <p className="text-muted-foreground p-4 text-center text-sm">
                  {strings.targetStep.loadingTargets}
                </p>
              ) : (
                shownWorkloads.length === 0 &&
                !workloads.isError && (
                  <p className="text-muted-foreground p-4 text-center text-sm">
                    {strings.targetStep.noTargets}
                  </p>
                )
              )}
              {shownWorkloads.map((path) => (
                <Option
                  key={path}
                  selected={
                    !draft.targetless &&
                    draft.target?.path === path &&
                    draft.target.namespace === namespace
                  }
                  onSelect={() => selectWorkload(path)}
                >
                  <span className="font-medium">{withoutKind(path)}</span>
                  <Badge variant="outline" className="text-xs">
                    {workloadKind(path)}
                  </Badge>
                </Option>
              ))}
              {mode?.targetless && (
                <Option selected={draft.targetless} onSelect={selectTargetless}>
                  <Unplug className="text-muted-foreground h-4 w-4" />
                  <span className="font-medium">
                    {strings.targetStep.targetless}
                  </span>
                </Option>
              )}
            </div>
          </div>
        </div>

        {(draft.target !== null || draft.targetless) && (
          <div className="space-y-2">
            <Label htmlFor="service-name" className="text-sm font-medium">
              {strings.targetStep.serviceName}
            </Label>
            {draft.targetless ? (
              <>
                <Input
                  id="service-name"
                  value={draft.name}
                  onChange={(e) => setDraft({ ...draft, name: e.target.value })}
                />
                {nameProblem && (
                  <p className="text-destructive text-sm">{nameProblem}</p>
                )}
              </>
            ) : (
              <p id="service-name" className="font-mono text-sm">
                {draft.name}
              </p>
            )}
          </div>
        )}
      </div>
    </div>
  )
}

const Option = ({
  selected,
  onSelect,
  children,
}: {
  selected: boolean
  onSelect: () => void
  children: React.ReactNode
}) => (
  <button
    type="button"
    aria-pressed={selected}
    className={`flex w-full cursor-pointer items-center justify-between rounded-lg p-3 text-left transition-all duration-150 ${
      selected ? 'bg-primary/10 border-primary/20 border' : 'hover:bg-muted/50'
    }`}
    onClick={onSelect}
  >
    <span className="flex items-center gap-3">{children}</span>
    {selected && <Check className="text-primary h-4 w-4" />}
  </button>
)

export default TargetStep
