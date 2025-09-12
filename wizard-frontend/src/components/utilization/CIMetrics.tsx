import { GitBranch, Play, Clock, BarChart3 } from "lucide-react";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

interface CIMetricsProps {
  data: {
    currentRunningSessions: number;
    maxConcurrentCISessions: number;
    totalCISessions: number;
    avgCISessionDuration: number;
  };
}

export const CIMetrics = ({ data }: CIMetricsProps) => {
  const formatDuration = (minutes: number) => {
    const hours = Math.floor(minutes / 60);
    const mins = minutes % 60;
    if (hours > 0) {
      return `${hours}h ${mins}m`;
    }
    return `${mins}m`;
  };

  return (
    <div className="space-y-4">
      <h2 className="text-xl font-semibold mb-4">CI/CD Usage</h2>
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-4 gap-4">
        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Running CI Sessions
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              <Play className="h-4 w-4 text-green-500" />
              <span className="text-2xl font-bold">{data.currentRunningSessions}</span>
              <span className="text-sm text-muted-foreground">active</span>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Max CI Concurrent
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              <BarChart3 className="h-4 w-4 text-primary" />
              <span className="text-2xl font-bold">{data.maxConcurrentCISessions}</span>
              <span className="text-sm text-muted-foreground">peak</span>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Total CI Sessions
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              <GitBranch className="h-4 w-4 text-primary" />
              <span className="text-2xl font-bold">{data.totalCISessions}</span>
              <span className="text-sm text-muted-foreground">total</span>
            </div>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="pb-2">
            <CardTitle className="text-sm font-medium text-muted-foreground">
              Avg CI Duration
            </CardTitle>
          </CardHeader>
          <CardContent>
            <div className="flex items-center gap-2">
              <Clock className="h-4 w-4 text-primary" />
              <span className="text-2xl font-bold">{formatDuration(data.avgCISessionDuration)}</span>
              <span className="text-sm text-muted-foreground">avg</span>
            </div>
          </CardContent>
        </Card>
      </div>
    </div>
  );
};