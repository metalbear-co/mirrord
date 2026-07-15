import { useState, useRef, useEffect } from 'react'
import { Server, AlertCircle, ChevronDown, Search, Check } from 'lucide-react'
import {
  Button,
  Badge,
  Input,
  Label,
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from '@metalbear/ui'
import { readCurrentTargetDetails, updateConfigPorts, updateConfigTarget } from '../JsonUtils'
import { useConfigData } from '../UserDataContext'
import { useQuery } from '@tanstack/react-query'
import ALL_API_ROUTES from '../../lib/routes'

const QUERY_STALE_TIME_MS = 30000
const CLICK_OUTSIDE_DELAY_MS = 10

interface Target {
  target_path: string
  target_namespace: string
  containers: string[]
  detected_ports: number[]
}

interface NamespacesResponse {
  namespaces: string[]
}

interface TargetTypesResponse {
  targetTypes: string[]
}

interface KubeContext {
  name: string
  namespace: string | null
}

interface ContextsResponse {
  contexts: KubeContext[]
  current: string | null
}

const TargetTab = ({ setTargetPorts }: { setTargetPorts: (ports: number[]) => void }) => {
  const { config, setConfig } = useConfigData()
  const [selectedContext, setSelectedContext] = useState<string | undefined>(undefined)
  const [namespace, setNamespace] = useState<string>('default')
  const [targetType, setTargetType] = useState<string>('all')
  const [targetSearchText, setTargetSearchText] = useState<string>('')
  const [containerSearchText, setContainerSearchText] = useState<string>('')
  const [targetDropdownOpen, setTargetDropdownOpen] = useState(false)
  const [containerDropdownOpen, setContainerDropdownOpen] = useState(false)
  const targetDropdownRef = useRef<HTMLDivElement>(null)
  const containerDropdownRef = useRef<HTMLDivElement>(null)
  const targetSearchRef = useRef<HTMLInputElement>(null)
  const containerSearchRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (targetDropdownOpen) {
      targetSearchRef.current?.focus()
    }
  }, [targetDropdownOpen])

  useEffect(() => {
    if (containerDropdownOpen) {
      containerSearchRef.current?.focus()
    }
  }, [containerDropdownOpen])

  // Close dropdown when clicking outside
  useEffect(() => {
    if (!targetDropdownOpen && !containerDropdownOpen) return

    const handleClickOutside = (event: MouseEvent) => {
      if (targetDropdownRef.current && !targetDropdownRef.current.contains(event.target as Node)) {
        setTargetDropdownOpen(false)
      }
      if (
        containerDropdownRef.current &&
        !containerDropdownRef.current.contains(event.target as Node)
      ) {
        setContainerDropdownOpen(false)
      }
    }

    // Small delay to avoid closing immediately
    const timeoutId = setTimeout(() => {
      document.addEventListener('click', handleClickOutside)
    }, CLICK_OUTSIDE_DELAY_MS)

    return () => {
      clearTimeout(timeoutId)
      document.removeEventListener('click', handleClickOutside)
    }
  }, [targetDropdownOpen, containerDropdownOpen])

  const contextsQuery = useQuery<ContextsResponse>({
    staleTime: QUERY_STALE_TIME_MS,
    queryKey: ['kubeContexts'],
    queryFn: () =>
      fetch(window.location.origin + ALL_API_ROUTES.contexts).then(async (res) =>
        res.ok ? ((await res.json()) as ContextsResponse) : { contexts: [], current: null },
      ),
  })
  const availableContexts = contextsQuery.data?.contexts.map((c) => c.name) ?? []
  // Until the user picks a context, follow the kubeconfig's current one (the server also falls
  // back to it when the param is absent, so the picker and the queries stay in agreement).
  const context = selectedContext ?? contextsQuery.data?.current ?? undefined

  const handleContextChange = (value: string) => {
    setSelectedContext(value)
    setNamespace('default')
  }

  const namespacesQuery = useQuery<NamespacesResponse>({
    staleTime: QUERY_STALE_TIME_MS,
    queryKey: ['kubeNamespaces', context],
    queryFn: () =>
      fetch(window.location.origin + ALL_API_ROUTES.namespaces(context)).then(async (res) =>
        res.ok ? ((await res.json()) as NamespacesResponse) : { namespaces: [] },
      ),
  })

  const targetTypesQuery = useQuery<TargetTypesResponse>({
    staleTime: QUERY_STALE_TIME_MS,
    queryKey: ['kubeTargetTypes'],
    queryFn: () =>
      fetch(window.location.origin + ALL_API_ROUTES.targetTypes).then(async (res) =>
        res.ok ? ((await res.json()) as TargetTypesResponse) : { targetTypes: [] },
      ),
  })

  const availableNamespaces: string[] =
    namespacesQuery.isLoading || namespacesQuery.error
      ? []
      : (namespacesQuery.data?.namespaces ?? [])

  const availableTargetTypes: string[] =
    targetTypesQuery.isLoading || targetTypesQuery.error
      ? []
      : (targetTypesQuery.data?.targetTypes ?? [])

  const targetsQuery = useQuery<Target[]>({
    queryKey: ['targetDetails', context, namespace, targetType],
    queryFn: () =>
      fetch(
        window.location.origin +
          ALL_API_ROUTES.targets(namespace, targetType === 'all' ? undefined : targetType, context),
      ).then(async (res) => (res.ok ? ((await res.json()) as Target[]) : [])),
    enabled: !!namespace,
  })

  const availableTargets: Target[] =
    targetsQuery.isLoading || targetsQuery.error ? [] : (targetsQuery.data ?? [])

  const selectedTarget = readCurrentTargetDetails(config)
  const selectedTargetPath = selectedTarget.name
    ? `${selectedTarget.type}/${selectedTarget.name}`
    : undefined
  const selectedTargetInfo = availableTargets.find(
    (target) => target.target_path === selectedTargetPath,
  )
  const availableContainers = selectedTargetInfo?.containers ?? []
  const selectedContainer = selectedTarget.container ?? availableContainers[0]

  const handleTargetSelect = (target: Target) => {
    const defaultContainer = target.containers[0]
    const updated = updateConfigTarget(
      config,
      target.target_path,
      target.target_namespace,
      defaultContainer,
    )
    setTargetPorts(target.detected_ports)
    const updatedPorts = updateConfigPorts(target.detected_ports, updated)
    setConfig(updatedPorts)
    setContainerSearchText('')
    setTargetDropdownOpen(false)
  }

  const handleContainerSelect = (container: string) => {
    if (!selectedTargetPath) {
      return
    }

    const updated = updateConfigTarget(
      config,
      selectedTargetPath,
      selectedTargetInfo?.target_namespace ?? namespace,
      container,
    )
    setConfig(updated)
    setContainerDropdownOpen(false)
  }

  const filteredTargets = availableTargets.filter((target) =>
    target.target_path.toLowerCase().includes(targetSearchText.toLowerCase()),
  )
  const filteredContainers = availableContainers.filter((container) =>
    container.toLowerCase().includes(containerSearchText.toLowerCase()),
  )

  return (
    <div className="space-y-6">
      <div className="border-border flex items-center gap-3 border-b pb-4">
        <div className="bg-primary/10 flex h-10 w-10 items-center justify-center rounded-lg">
          <Server className="text-primary h-5 w-5" />
        </div>
        <div>
          <h3 className="text-lg font-semibold">Target Selection</h3>
          <p className="text-muted-foreground text-sm">
            Choose the Kubernetes resource to connect to
          </p>
        </div>
      </div>
      <div className="space-y-5">
        {availableContexts.length > 0 && (
          <div className="space-y-2">
            <Label htmlFor="kube-context" className="text-sm font-medium">
              Kube Context
            </Label>
            <Select
              {...(context !== undefined ? { value: context } : {})}
              onValueChange={handleContextChange}
            >
              <SelectTrigger className="h-10">
                <SelectValue placeholder="Current context" />
              </SelectTrigger>
              <SelectContent className="bg-card border-border border">
                {availableContexts.map((ctx) => (
                  <SelectItem key={ctx} value={ctx}>
                    {ctx}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        )}
        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label htmlFor="namespace" className="text-sm font-medium">
              Namespace
            </Label>
            <Select value={namespace} onValueChange={setNamespace}>
              <SelectTrigger className="h-10">
                <SelectValue placeholder="Select namespace" />
              </SelectTrigger>
              <SelectContent className="bg-card border-border border">
                {availableNamespaces.length === 0 ? (
                  <SelectItem value="default" disabled>
                    No namespaces available
                  </SelectItem>
                ) : (
                  availableNamespaces.map((ns) => (
                    <SelectItem key={ns} value={ns}>
                      {ns}
                    </SelectItem>
                  ))
                )}
              </SelectContent>
            </Select>
          </div>

          <div className="space-y-2">
            <Label htmlFor="target-type" className="text-sm font-medium">
              Resource Type
            </Label>
            <Select value={targetType} onValueChange={setTargetType}>
              <SelectTrigger className="h-10">
                <SelectValue placeholder="All types" />
              </SelectTrigger>
              <SelectContent className="bg-card border-border border">
                <SelectItem value="all">All types</SelectItem>
                {availableTargetTypes.map((type) => (
                  <SelectItem key={type} value={type}>
                    {type}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>

        <div className="space-y-2">
          <Label htmlFor="target-search" className="text-sm font-medium">
            Target
          </Label>
          <div className="relative" ref={targetDropdownRef}>
            <Button
              variant="outline"
              className="hover:bg-muted/50 h-10 w-full justify-between font-normal"
              onClick={() => setTargetDropdownOpen(!targetDropdownOpen)}
              type="button"
            >
              {selectedTarget.name ? (
                <span className="flex items-center gap-2">
                  <span className="font-medium">{selectedTarget.name}</span>
                  <Badge
                    variant="outline"
                    className="bg-primary/5 border-primary/20 text-primary text-xs"
                  >
                    {selectedTarget.type}
                  </Badge>
                </span>
              ) : (
                <span className="text-muted-foreground">Select a target...</span>
              )}
              <ChevronDown
                className={`text-muted-foreground h-4 w-4 transition-transform duration-200 ${targetDropdownOpen ? 'rotate-180' : ''}`}
              />
            </Button>

            {targetDropdownOpen && (
              <div className="border-border bg-card animate-scale-in absolute z-50 mt-2 w-full overflow-hidden rounded-xl border shadow-lg">
                <div className="border-border bg-muted/30 border-b p-3">
                  <div className="relative">
                    <Search className="text-muted-foreground absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2" />
                    <Input
                      ref={targetSearchRef}
                      placeholder="Search targets..."
                      className="bg-card h-9 pl-9"
                      value={targetSearchText}
                      onChange={(e) => setTargetSearchText(e.target.value)}
                    />
                  </div>
                </div>
                <div className="max-h-60 overflow-y-auto">
                  {targetsQuery.isLoading ? (
                    <div className="p-6 text-center">
                      <div className="spinner mx-auto mb-2 h-6 w-6" />
                      <p className="text-muted-foreground text-sm">Loading targets...</p>
                    </div>
                  ) : filteredTargets.length === 0 ? (
                    <div className="bg-muted/20 m-2 rounded-lg p-6 text-center">
                      <Server className="text-muted-foreground mx-auto mb-2 h-8 w-8 opacity-50" />
                      <p className="text-muted-foreground text-sm">No targets found</p>
                    </div>
                  ) : (
                    <div className="p-2">
                      {filteredTargets.map((target) => {
                        const isSelected = selectedTargetPath === target.target_path
                        return (
                          <div
                            key={`${target.target_namespace}/${target.target_path}`}
                            role="button"
                            tabIndex={0}
                            className={`flex w-full cursor-pointer items-center justify-between rounded-lg p-3 transition-all duration-150 ${
                              isSelected
                                ? 'bg-primary/10 border-primary/20 border'
                                : 'hover:bg-muted/50'
                            } `}
                            onClick={() => handleTargetSelect(target)}
                            onKeyDown={(e) => {
                              if (e.key === 'Enter' || e.key === ' ') {
                                e.preventDefault()
                                handleTargetSelect(target)
                              }
                            }}
                          >
                            <div className="flex items-center gap-3">
                              <span
                                className={`font-medium ${isSelected ? 'text-primary' : 'text-foreground'}`}
                              >
                                {target.target_path.split('/')[1]}
                              </span>
                              <Badge
                                variant="outline"
                                className={`text-xs ${isSelected ? 'bg-primary/10 border-primary/30 text-primary' : ''}`}
                              >
                                {target.target_path.split('/')[0]}
                              </Badge>
                            </div>
                            {isSelected && <Check className="text-primary h-4 w-4" />}
                          </div>
                        )
                      })}
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>

          {!config.target && (
            <p className="text-destructive bg-destructive/10 border-l-destructive border-destructive/10 mt-3 flex items-center gap-2 rounded-lg border border-l-2 p-3 text-sm">
              <AlertCircle className="h-4 w-4 flex-shrink-0 animate-pulse" />
              Please select a target to continue
            </p>
          )}
        </div>

        {selectedTarget.name && (
          <div className="space-y-2">
            <Label htmlFor="container-search" className="text-sm font-medium">
              Container
            </Label>
            <div className="relative" ref={containerDropdownRef}>
              <Button
                variant="outline"
                className="hover:bg-muted/50 h-10 w-full justify-between font-normal disabled:opacity-70"
                onClick={() => setContainerDropdownOpen(!containerDropdownOpen)}
                type="button"
                disabled={availableContainers.length === 0}
              >
                {selectedContainer ? (
                  <span className="font-medium">{selectedContainer}</span>
                ) : (
                  <span className="text-muted-foreground">No containers found</span>
                )}
                <ChevronDown
                  className={`text-muted-foreground h-4 w-4 transition-transform duration-200 ${containerDropdownOpen ? 'rotate-180' : ''}`}
                />
              </Button>

              {containerDropdownOpen && (
                <div className="border-border bg-card animate-scale-in absolute z-50 mt-2 w-full overflow-hidden rounded-xl border shadow-lg">
                  <div className="border-border bg-muted/30 border-b p-3">
                    <div className="relative">
                      <Search className="text-muted-foreground absolute left-3 top-1/2 h-4 w-4 -translate-y-1/2" />
                      <Input
                        ref={containerSearchRef}
                        placeholder="Search containers..."
                        className="bg-card h-9 pl-9"
                        value={containerSearchText}
                        onChange={(e) => setContainerSearchText(e.target.value)}
                      />
                    </div>
                  </div>
                  <div className="max-h-60 overflow-y-auto">
                    {filteredContainers.length === 0 ? (
                      <div className="bg-muted/20 m-2 rounded-lg p-6 text-center">
                        <p className="text-muted-foreground text-sm">No containers found</p>
                      </div>
                    ) : (
                      <div className="p-2">
                        {filteredContainers.map((container) => {
                          const isSelected = selectedContainer === container
                          return (
                            <div
                              key={`${selectedTargetPath}/${container}`}
                              role="button"
                              tabIndex={0}
                              className={`flex w-full cursor-pointer items-center justify-between rounded-lg p-3 transition-all duration-150 ${
                                isSelected
                                  ? 'bg-primary/10 border-primary/20 border'
                                  : 'hover:bg-muted/50'
                              } `}
                              onClick={() => handleContainerSelect(container)}
                              onKeyDown={(e) => {
                                if (e.key === 'Enter' || e.key === ' ') {
                                  e.preventDefault()
                                  handleContainerSelect(container)
                                }
                              }}
                            >
                              <span
                                className={`font-medium ${isSelected ? 'text-primary' : 'text-foreground'}`}
                              >
                                {container}
                              </span>
                              {isSelected && <Check className="text-primary h-4 w-4" />}
                            </div>
                          )
                        })}
                      </div>
                    )}
                  </div>
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    </div>
  )
}

export default TargetTab
