import { useState, useContext, useRef, useEffect } from "react";
import { Server, AlertCircle, ChevronDown, Search } from "lucide-react";
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
  Card,
  CardContent,
  CardHeader,
  CardTitle,
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
    <Card>
      <CardHeader className="pb-4">
        <CardTitle className="flex items-center gap-2 text-lg">
          <Server className="h-5 w-5" />
          Target Selection
        </CardTitle>
      </CardHeader>
      <CardContent className="space-y-4">
        <div className="space-y-2">
          <Label htmlFor="namespace">Namespace</Label>
          <Select value={namespace} onValueChange={setNamespace}>
            <SelectTrigger>
              <SelectValue placeholder="Select a namespace" />
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
          <Label htmlFor="target-type">Target Type</Label>
          <Select value={targetType} onValueChange={setTargetType}>
            <SelectTrigger>
              <SelectValue placeholder="All resource types" />
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

        <div className="space-y-2">
          <Label htmlFor="target-search">Choose Target</Label>
          <div className="relative" ref={containerRef}>
            <Button
              variant="outline"
              className="w-full justify-between font-normal"
              onClick={() => setDropdownOpen(!dropdownOpen)}
              type="button"
            >
              {selectedTarget.name ? (
                <span className="flex items-center gap-2">
                  <span>{selectedTarget.name}</span>
                  <Badge variant="outline" className="text-xs">
                    {selectedTarget.type}
                  </Badge>
                </span>
              ) : (
                <span className="text-[var(--muted-foreground)]">
                  Search for target...
                </span>
              )}
              <ChevronDown className={`h-4 w-4 opacity-50 transition-transform ${dropdownOpen ? 'rotate-180' : ''}`} />
            </Button>

            {dropdownOpen && (
              <div className="absolute z-50 w-full mt-1 rounded-lg border border-[var(--border)] bg-[var(--card)] shadow-lg">
                <div className="p-3 border-b border-[var(--border)]">
                  <div className="relative">
                    <Search className="absolute left-3 top-1/2 -translate-y-1/2 h-4 w-4 text-[var(--muted-foreground)]" />
                    <Input
                      placeholder="Search targets..."
                      className="pl-9"
                      value={targetSearchText}
                      onChange={(e) => setTargetSearchText(e.target.value)}
                      autoFocus
                    />
                  </div>
                </div>
                <div className="max-h-60 overflow-y-auto p-2">
                  {targetsQuery.isLoading ? (
                    <div className="p-4 text-center text-sm text-[var(--muted-foreground)]">
                      Loading targets...
                    </div>
                  ) : filteredTargets.length === 0 ? (
                    <div className="p-4 text-center text-sm text-[var(--muted-foreground)]">
                      No targets found
                    </div>
                  ) : (
                    <div className="space-y-1">
                      {filteredTargets.map((target) => (
                        <div
                          key={`${target.target_namespace}/${target.target_path}`}
                          className="w-full flex items-center justify-between p-2 rounded-md hover:bg-[var(--muted)] transition-colors text-left cursor-pointer"
                          onClick={() => handleTargetSelect(target)}
                        >
                          <span className="font-medium text-[var(--foreground)]">
                            {target.target_path.split("/")[1]}
                          </span>
                          <Badge variant="outline" className="text-xs">
                            {target.target_path.split("/")[0]}
                          </Badge>
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>
            )}
          </div>

          {!config.target && (
            <p className="text-sm text-destructive flex items-center gap-1 mt-2">
              <AlertCircle className="h-4 w-4" />
              Please select a target to continue
            </p>
          )}
        </div>
      </CardContent>
    </Card>
  );
};

export default TargetTab;
