import { useState, useContext, useRef, useEffect } from "react";
import { Server, AlertCircle, ChevronDown, Search, Check } from "lucide-react";
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
} from "@metalbear/ui";
import {
  readCurrentTargetDetails,
  updateConfigPorts,
  updateConfigTarget,
} from "../JsonUtils";
import { ConfigDataContext } from "../UserDataContext";
import { useQuery } from "@tanstack/react-query";
import ALL_API_ROUTES from "../../lib/routes";

interface Target {
  target_path: string;
  target_namespace: string;
  detected_ports: number[];
}

interface ClusterDetails {
  namespaces: string[];
  target_types: string[];
}

const TargetTab = ({
  setTargetPorts,
}: {
  setTargetPorts: (ports: number[]) => void;
}) => {
  const { config, setConfig } = useContext(ConfigDataContext)!;
  const [namespace, setNamespace] = useState<string>("default");
  const [targetType, setTargetType] = useState<string>("all");
  const [targetSearchText, setTargetSearchText] = useState<string>("");
  const [dropdownOpen, setDropdownOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement>(null);

  // Close dropdown when clicking outside
  useEffect(() => {
    if (!dropdownOpen) return;

    const handleClickOutside = (event: MouseEvent) => {
      if (containerRef.current && !containerRef.current.contains(event.target as Node)) {
        setDropdownOpen(false);
      }
    };

    // Small delay to avoid closing immediately
    const timeoutId = setTimeout(() => {
      document.addEventListener("click", handleClickOutside);
    }, 10);

    return () => {
      clearTimeout(timeoutId);
      document.removeEventListener("click", handleClickOutside);
    };
  }, [dropdownOpen]);

  const clusterDetailsQuery = useQuery<ClusterDetails>({
    staleTime: 30 * 1000,
    queryKey: ["clusterDetails"],
    queryFn: () =>
      fetch(window.location.origin + ALL_API_ROUTES.clusterDetails).then(
        async (res) =>
          res.ok ? await res.json() : { namespaces: [], target_types: [] }
      ),
  });

  const availableNamespaces: string[] =
    clusterDetailsQuery.isLoading || clusterDetailsQuery.error
      ? []
      : clusterDetailsQuery.data?.namespaces ?? [];

  const availableTargetTypes: string[] =
    clusterDetailsQuery.isLoading || clusterDetailsQuery.error
      ? []
      : clusterDetailsQuery.data?.target_types ?? [];

  const targetsQuery = useQuery<Target[]>({
    queryKey: ["targetDetails", namespace, targetType],
    queryFn: () =>
      fetch(
        window.location.origin + ALL_API_ROUTES.targets(namespace, targetType === "all" ? undefined : targetType)
      ).then(async (res) => (res.ok ? await res.json() : [])),
    enabled: !!namespace,
  });

  const availableTargets: Target[] =
    targetsQuery.isLoading || targetsQuery.error ? [] : targetsQuery.data ?? [];

  const selectedTarget = readCurrentTargetDetails(config);

  const handleTargetSelect = (target: Target) => {
    const updated = updateConfigTarget(
      config,
      target.target_path,
      target.target_namespace
    );
    setTargetPorts(target.detected_ports);
    const updatedPorts = updateConfigPorts(target.detected_ports, updated);
    setConfig(updatedPorts);
    setDropdownOpen(false);
  };

  const filteredTargets = availableTargets.filter((target) =>
    target.target_path.toLowerCase().includes(targetSearchText.toLowerCase())
  );

  return (
    <div className="space-y-6">
      <div className="flex items-center gap-3 pb-4 border-b border-[var(--border)]">
        <div className="w-10 h-10 rounded-lg bg-primary/10 flex items-center justify-center">
          <Server className="h-5 w-5 text-primary" />
        </div>
        <div>
          <h3 className="text-lg font-semibold">Target Selection</h3>
          <p className="text-sm text-[var(--muted-foreground)]">
            Choose the Kubernetes resource to connect to
          </p>
        </div>
      </div>
      <div className="space-y-5">
        <div className="grid grid-cols-2 gap-4">
          <div className="space-y-2">
            <Label htmlFor="namespace" className="text-sm font-medium">
              Namespace
            </Label>
            <Select value={namespace} onValueChange={setNamespace}>
              <SelectTrigger className="h-10">
                <SelectValue placeholder="Select namespace" />
              </SelectTrigger>
              <SelectContent className="bg-[var(--card)] border border-[var(--border)]">
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
              <SelectContent className="bg-[var(--card)] border border-[var(--border)]">
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
          <div className="relative" ref={containerRef}>
            <Button
              variant="outline"
              className="w-full h-10 justify-between font-normal hover:bg-[var(--muted)]/50"
              onClick={() => setDropdownOpen(!dropdownOpen)}
              type="button"
            >
              {selectedTarget.name ? (
                <span className="flex items-center gap-2">
                  <span className="font-medium">{selectedTarget.name}</span>
                  <Badge variant="outline" className="text-xs bg-primary/5 border-primary/20 text-primary">
                    {selectedTarget.type}
                  </Badge>
                </span>
              ) : (
                <span className="text-[var(--muted-foreground)]">
                  Select a target...
                </span>
              )}
              <ChevronDown className={`h-4 w-4 text-[var(--muted-foreground)] transition-transform duration-200 ${dropdownOpen ? 'rotate-180' : ''}`} />
            </Button>

            {dropdownOpen && (
              <div className="absolute z-50 w-full mt-2 rounded-xl border border-[var(--border)] bg-[var(--card)] shadow-lg overflow-hidden animate-scale-in">
                <div className="p-3 border-b border-[var(--border)] bg-[var(--muted)]/30">
                  <div className="relative">
                    <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-[var(--muted-foreground)]" />
                    <Input
                      placeholder="Search targets..."
                      className="pl-9 h-9 bg-[var(--card)]"
                      value={targetSearchText}
                      onChange={(e) => setTargetSearchText(e.target.value)}
                      autoFocus
                    />
                  </div>
                </div>
                <div className="max-h-60 overflow-y-auto">
                  {targetsQuery.isLoading ? (
                    <div className="p-6 text-center">
                      <div className="w-6 h-6 border-2 border-primary border-t-transparent rounded-full animate-spin mx-auto mb-2" />
                      <p className="text-sm text-[var(--muted-foreground)]">Loading targets...</p>
                    </div>
                  ) : filteredTargets.length === 0 ? (
                    <div className="p-6 text-center">
                      <Server className="h-8 w-8 text-[var(--muted-foreground)] mx-auto mb-2 opacity-50" />
                      <p className="text-sm text-[var(--muted-foreground)]">No targets found</p>
                    </div>
                  ) : (
                    <div className="p-2">
                      {filteredTargets.map((target) => {
                        const isSelected = selectedTarget.name === target.target_path.split("/")[1];
                        return (
                          <div
                            key={`${target.target_namespace}/${target.target_path}`}
                            className={`
                              w-full flex items-center justify-between p-3 rounded-lg transition-all duration-150 cursor-pointer
                              ${isSelected
                                ? "bg-primary/10 border border-primary/20"
                                : "hover:bg-[var(--muted)]/50"
                              }
                            `}
                            onClick={() => handleTargetSelect(target)}
                          >
                            <div className="flex items-center gap-3">
                              <span className={`font-medium ${isSelected ? "text-primary" : "text-[var(--foreground)]"}`}>
                                {target.target_path.split("/")[1]}
                              </span>
                              <Badge
                                variant="outline"
                                className={`text-xs ${isSelected ? "bg-primary/10 border-primary/30 text-primary" : ""}`}
                              >
                                {target.target_path.split("/")[0]}
                              </Badge>
                            </div>
                            {isSelected && (
                              <Check className="h-4 w-4 text-primary" />
                            )}
                          </div>
                        );
                      })}
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>

          {!config.target && (
            <p className="text-sm text-destructive flex items-center gap-2 mt-3 p-3 rounded-lg bg-destructive/5 border border-destructive/10">
              <AlertCircle className="h-4 w-4 flex-shrink-0" />
              Please select a target to continue
            </p>
          )}
        </div>
      </div>
    </div>
  );
};

export default TargetTab;
