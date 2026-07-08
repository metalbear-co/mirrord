import { useState, useContext, useRef, useEffect } from 'react'
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
import {
  readCurrentTargetDetails,
  updateConfigPorts,
  updateConfigTarget,
} from '../JsonUtils'
import { ConfigDataContext } from '../UserDataContext'
import { useQuery } from '@tanstack/react-query'
import ALL_API_ROUTES from '../../lib/routes'

interface Target {
  target_path: string
  target_namespace: string
  containers: string[]
  detected_ports: number[]
}

interface ClusterDetails {
  namespaces: string[]
  target_types: string[]
}

interface ContextsResponse {
  contexts: string[]
  currentContext: string | null
}

const TargetTab = ({
  setTargetPorts,
}: {
  setTargetPorts: (ports: number[]) => void
}) => {
  const { config, setConfig } = useContext(ConfigDataContext)!
  const [selectedContext, setSelectedContext] = useState<string | undefined>(
    undefined,
  )
  const [namespace, setNamespace] = useState<string>('default')
  const [targetType, setTargetType] = useState<string>('all')
  const [targetSearchText, setTargetSearchText] = useState<string>('')
  const [containerSearchText, setContainerSearchText] = useState<string>('')
  const [targetDropdownOpen, setTargetDropdownOpen] = useState(false)
  const [containerDropdownOpen, setContainerDropdownOpen] = useState(false)
  const targetDropdownRef = useRef<HTMLDivElement>(null)
  const containerDropdownRef = useRef<HTMLDivElement>(null)

  // Close dropdown when clicking outside
  useEffect(() => {
    if (!targetDropdownOpen && !containerDropdownOpen) return

    const handleClickOutside = (event: MouseEvent) => {
      if (
        targetDropdownRef.current &&
        !targetDropdownRef.current.contains(event.target as Node)
      ) {
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
    }, 10)

    return () => {
      clearTimeout(timeoutId)
      document.removeEventListener('click', handleClickOutside)
    }
  }, [targetDropdownOpen, containerDropdownOpen])

  const contextsQuery = useQuery<ContextsResponse>({
    staleTime: 30 * 1000,
    queryKey: ['kubeContexts'],
    queryFn: () =>
      fetch(window.location.origin + ALL_API_ROUTES.contexts).then(
        async (res) =>
          res.ok ? await res.json() : { contexts: [], currentContext: null },
      ),
  })
  const availableContexts = contextsQuery.data?.contexts ?? []
  // Until the user picks a context, follow the kubeconfig's current one (the server also falls
  // back to it when the param is absent, so the picker and the queries stay in agreement).
  const context =
    selectedContext ?? contextsQuery.data?.currentContext ?? undefined

  const handleContextChange = (value: string) => {
    setSelectedContext(value)
    setNamespace('default')
  }

  const clusterDetailsQuery = useQuery<ClusterDetails>({
    staleTime: 30 * 1000,
    queryKey: ['clusterDetails', context],
    queryFn: () =>
      fetch(
        window.location.origin + ALL_API_ROUTES.clusterDetails(context),
      ).then(async (res) =>
        res.ok ? await res.json() : { namespaces: [], target_types: [] },
      ),
  })

  const availableNamespaces: string[] =
    clusterDetailsQuery.isLoading || clusterDetailsQuery.error
      ? []
      : (clusterDetailsQuery.data?.namespaces ?? [])

  const availableTargetTypes: string[] =
    clusterDetailsQuery.isLoading || clusterDetailsQuery.error
      ? []
      : (clusterDetailsQuery.data?.target_types ?? [])

  const targetsQuery = useQuery<Target[]>({
    queryKey: ['targetDetails', context, namespace, targetType],
    queryFn: () =>
      fetch(
        window.location.origin +
          ALL_API_ROUTES.targets(
            namespace,
            targetType === 'all' ? undefined : targetType,
            context,
          ),
      ).then(async (res) => (res.ok ? await res.json() : [])),
    enabled: !!namespace,
  })

  const availableTargets: Target[] =
    targetsQuery.isLoading || targetsQuery.error
      ? []
      : (targetsQuery.data ?? [])

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
      <div className="flex items-center gap-3 pb-4 border-b border-border">
        <div className="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center">
          <Server className="h-5 w-5 text-primary" />
        </div>
        <div>
          <h3 className="text-lg font-semibold">Target Selection</h3>
          <p className="text-sm text-muted-foreground">
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
            <Select value={context} onValueChange={handleContextChange}>
              <SelectTrigger className="h-10">
                <SelectValue placeholder="Current context" />
              </SelectTrigger>
              <SelectContent className="bg-card border border-border">
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
              <SelectContent className="bg-card border border-border">
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
              <SelectContent className="bg-card border border-border">
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
              className="w-full h-10 justify-between font-normal hover:bg-muted/50"
              onClick={() => setTargetDropdownOpen(!targetDropdownOpen)}
              type="button"
            >
              {selectedTarget.name ? (
                <span className="flex items-center gap-2">
                  <span className="font-medium">{selectedTarget.name}</span>
                  <Badge
                    variant="outline"
                    className="text-xs bg-primary/5 border-primary/20 text-primary"
                  >
                    {selectedTarget.type}
                  </Badge>
                </span>
              ) : (
                <span className="text-muted-foreground">
                  Select a target...
                </span>
              )}
              <ChevronDown
                className={`h-4 w-4 text-muted-foreground transition-transform duration-200 ${targetDropdownOpen ? 'rotate-180' : ''}`}
              />
            </Button>

            {targetDropdownOpen && (
              <div className="absolute z-50 w-full mt-2 rounded-xl border border-border bg-card shadow-lg overflow-hidden animate-scale-in">
                <div className="p-3 border-b border-border bg-muted/30">
                  <div className="relative">
                    <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                    <Input
                      placeholder="Search targets..."
                      className="pl-9 h-9 bg-card"
                      value={targetSearchText}
                      onChange={(e) => setTargetSearchText(e.target.value)}
                      autoFocus
                    />
                  </div>
                </div>
                <div className="max-h-60 overflow-y-auto">
                  {targetsQuery.isLoading ? (
                    <div className="p-6 text-center">
                      <div className="w-6 h-6 spinner mx-auto mb-2" />
                      <p className="text-sm text-muted-foreground">
                        Loading targets...
                      </p>
                    </div>
                  ) : filteredTargets.length === 0 ? (
                    <div className="p-6 text-center bg-muted/20 m-2 rounded-lg">
                      <Server className="h-8 w-8 text-muted-foreground mx-auto mb-2 opacity-50" />
                      <p className="text-sm text-muted-foreground">
                        No targets found
                      </p>
                    </div>
                  ) : (
                    <div className="p-2">
                      {filteredTargets.map((target) => {
                        const isSelected =
                          selectedTargetPath === target.target_path
                        return (
                          <div
                            key={`${target.target_namespace}/${target.target_path}`}
                            className={`
                              w-full flex items-center justify-between p-3 rounded-lg transition-all duration-150 cursor-pointer
                              ${
                                isSelected
                                  ? 'bg-primary/10 border border-primary/20'
                                  : 'hover:bg-muted/50'
                              }
                            `}
                            onClick={() => handleTargetSelect(target)}
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
                            {isSelected && (
                              <Check className="h-4 w-4 text-primary" />
                            )}
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
            <p className="text-sm text-destructive flex items-center gap-2 mt-3 p-3 rounded-lg bg-destructive/10 border-l-2 border-l-destructive border border-destructive/10">
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
                className="w-full h-10 justify-between font-normal hover:bg-muted/50 disabled:opacity-70"
                onClick={() => setContainerDropdownOpen(!containerDropdownOpen)}
                type="button"
                disabled={availableContainers.length === 0}
              >
                {selectedContainer ? (
                  <span className="font-medium">{selectedContainer}</span>
                ) : (
                  <span className="text-muted-foreground">
                    No containers found
                  </span>
                )}
                <ChevronDown
                  className={`h-4 w-4 text-muted-foreground transition-transform duration-200 ${containerDropdownOpen ? 'rotate-180' : ''}`}
                />
              </Button>

              {containerDropdownOpen && (
                <div className="absolute z-50 w-full mt-2 rounded-xl border border-border bg-card shadow-lg overflow-hidden animate-scale-in">
                  <div className="p-3 border-b border-border bg-muted/30">
                    <div className="relative">
                      <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-muted-foreground" />
                      <Input
                        placeholder="Search containers..."
                        className="pl-9 h-9 bg-card"
                        value={containerSearchText}
                        onChange={(e) => setContainerSearchText(e.target.value)}
                        autoFocus
                      />
                    </div>
                  </div>
                  <div className="max-h-60 overflow-y-auto">
                    {filteredContainers.length === 0 ? (
                      <div className="p-6 text-center bg-muted/20 m-2 rounded-lg">
                        <p className="text-sm text-muted-foreground">
                          No containers found
                        </p>
                      </div>
                    ) : (
                      <div className="p-2">
                        {filteredContainers.map((container) => {
                          const isSelected = selectedContainer === container
                          return (
                            <div
                              key={`${selectedTargetPath}/${container}`}
                              className={`
                                w-full flex items-center justify-between p-3 rounded-lg transition-all duration-150 cursor-pointer
                                ${
                                  isSelected
                                    ? 'bg-primary/10 border border-primary/20'
                                    : 'hover:bg-muted/50'
                                }
                              `}
                              onClick={() => handleContainerSelect(container)}
                            >
                              <span
                                className={`font-medium ${isSelected ? 'text-primary' : 'text-foreground'}`}
                              >
                                {container}
                              </span>
                              {isSelected && (
                                <Check className="h-4 w-4 text-primary" />
                              )}
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
