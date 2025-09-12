import { Users, Activity, TrendingUp, Timer, ExternalLink } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

interface TimeframeMetricsProps {
  generalData: {
    totalLicenses: number;
    usedMachines: number;
    currentConcurrentSessions: number;
    maxConcurrentSessions: number;
    developmentTimeSaved: number;
    totalSessionTime: number; // in hours
  };
  ciData: {
    currentRunningSessions: number;
    maxConcurrentCISessions: number;
  };
  filters: {
    from?: string;
    to?: string;
  };
}

export const TimeframeMetrics = ({ generalData, ciData, filters }: TimeframeMetricsProps) => {
  const formatTimeSaved = (hours: number) => {
    const years = Math.floor(hours / (365 * 24));
    const months = Math.floor((hours % (365 * 24)) / (30 * 24));
    const weeks = Math.floor((hours % (30 * 24)) / (7 * 24));
    const days = Math.floor((hours % (7 * 24)) / 24);
    const remainingHours = Math.floor(hours % 24);

    const parts = [];
    if (years > 0) parts.push(`${years}y`);
    if (months > 0) parts.push(`${months}m`);
    if (weeks > 0) parts.push(`${weeks}w`);
    if (days > 0) parts.push(`${days}d`);
    if (remainingHours > 0) parts.push(`${remainingHours}h`);

    // Return only the 2 highest time frames
    return parts.slice(0, 2).join(' ') || '0h';
  };

  const formatDateRange = () => {
    if (!filters.from || !filters.to) return "Selected Period";
    const from = new Date(filters.from).toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
    const to = new Date(filters.to).toLocaleDateString('en-US', { month: 'short', day: 'numeric' });
    return `${from} - ${to}`;
  };

  const ciSessionsInPeriod = Math.floor(ciData.currentRunningSessions * 0.7);

  return (
    <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-2">
      <Card className="bg-gradient-card border-border/50">
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-sm font-medium text-muted-foreground">Licenses</CardTitle>
            <Users className="h-4 w-4 text-muted-foreground" />
          </div>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="bg-muted/30 rounded-lg p-2 mb-1 text-center">
            <div className="flex items-baseline justify-center gap-2">
              <span className="text-2xl font-bold">{Math.floor(generalData.usedMachines * 0.6)}</span>
              <span className="text-lg font-semibold text-muted-foreground">/</span>
              <span className="text-2xl font-bold">50</span>
            </div>
          </div>
          <div className="text-xs text-muted-foreground text-center">
            avg used / acquired licenses
          </div>
        </CardContent>
      </Card>

      <Card className="bg-gradient-card border-border/50">
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-sm font-medium text-muted-foreground">Active Sessions</CardTitle>
            <Activity className="h-4 w-4 text-green-500" />
          </div>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="space-y-1">
            <div className="bg-muted/30 rounded-lg p-2 text-center">
              <div className="flex items-baseline justify-center gap-1">
                <span className="text-2xl font-bold">{Math.floor(generalData.currentConcurrentSessions * 0.8)}</span>
                <span className="text-sm text-muted-foreground">user</span>
              </div>
            </div>
            <div className="bg-muted/30 rounded-lg p-2 text-center">
              <div className="flex items-baseline justify-center gap-1">
                <span className="text-2xl font-bold">0</span>
                <span className="text-sm text-muted-foreground">CI</span>
              </div>
            </div>
          </div>
          <a 
            href="https://mirrord.dev/docs/overview/ci/" 
            target="_blank" 
            rel="noopener noreferrer"
            className="flex items-center gap-1 text-xs text-primary hover:text-primary/80 mt-2"
          >
            Learn more about CI
            <ExternalLink className="h-3 w-3" />
          </a>
        </CardContent>
      </Card>

      <Card className="bg-gradient-card border-border/50">
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-sm font-medium text-muted-foreground">Max Concurrency</CardTitle>
            <TrendingUp className="h-4 w-4 text-muted-foreground" />
          </div>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="space-y-1">
            <div className="bg-muted/30 rounded-lg p-2 text-center">
              <div className="flex items-baseline justify-center gap-1">
                <span className="text-2xl font-bold">{Math.floor(generalData.maxConcurrentSessions * 0.7)}</span>
                <span className="text-sm text-muted-foreground">user</span>
              </div>
            </div>
            <div className="bg-muted/30 rounded-lg p-2 text-center">
              <div className="flex items-baseline justify-center gap-1">
                <span className="text-2xl font-bold">{Math.floor(ciData.maxConcurrentCISessions * 0.6)}</span>
                <span className="text-sm text-muted-foreground">CI</span>
              </div>
            </div>
          </div>
        </CardContent>
      </Card>

      <Card className="bg-gradient-card border-border/50">
        <CardHeader className="pb-2">
          <div className="flex items-center justify-between">
            <CardTitle className="text-sm font-medium text-muted-foreground">Total Session Time</CardTitle>
            <Timer className="h-4 w-4 text-muted-foreground" />
          </div>
        </CardHeader>
        <CardContent className="pt-0">
          <div className="bg-muted/30 rounded-lg p-2 mb-1 text-center">
            <div className="text-2xl font-bold">{formatTimeSaved(generalData.totalSessionTime * 0.4)}</div>
          </div>
          <div className="text-xs text-muted-foreground text-center">
            development time for period (months & weeks)
          </div>
        </CardContent>
      </Card>

    </div>
  );
};