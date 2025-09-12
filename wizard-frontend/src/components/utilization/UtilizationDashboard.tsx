import { useState, useEffect } from "react";
import { Calendar, Download, RefreshCw } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Skeleton } from "@/components/ui/skeleton";
import { useToast } from "@/hooks/use-toast";
import { UtilizationReport, UtilizationFilters } from "@/types/utilization";
import { fetchUtilizationReport, exportUtilizationReport } from "@/services/mockUtilizationApi";
import { GeneralMetrics } from "./GeneralMetrics";
import { TimeframeMetrics } from "./TimeframeMetrics";
import { UserMetricsTable } from "./UserMetricsTable";
import { UsageCharts } from "./UsageCharts";
import { DateRangePicker } from "./DateRangePicker";
import { TimeSavedCard } from "./TimeSavedCard";

export const UtilizationDashboard = () => {
  const [data, setData] = useState<UtilizationReport | null>(null);
  const [loading, setLoading] = useState(true);
  const [exporting, setExporting] = useState(false);
  const [filters, setFilters] = useState<UtilizationFilters>(() => {
    const now = new Date();
    const sevenDaysAgo = new Date(now.getTime() - 7 * 24 * 60 * 60 * 1000);
    return {
      from: sevenDaysAgo.toISOString().split('T')[0],
      to: now.toISOString().split('T')[0],
    };
  });
  const { toast } = useToast();

  const loadData = async () => {
    try {
      setLoading(true);
      const report = await fetchUtilizationReport(filters);
      setData(report);
    } catch (error) {
      toast({
        title: "Error",
        description: "Failed to load utilization data",
        variant: "destructive",
      });
    } finally {
      setLoading(false);
    }
  };

  const handleExport = async () => {
    try {
      setExporting(true);
      await exportUtilizationReport(filters);
      toast({
        title: "Export Started",
        description: "Your utilization report is being prepared for download",
      });
    } catch (error) {
      toast({
        title: "Export Failed",
        description: "Failed to export utilization data",
        variant: "destructive",
      });
    } finally {
      setExporting(false);
    }
  };

  const handleFiltersChange = (newFilters: UtilizationFilters) => {
    setFilters(newFilters);
  };

  useEffect(() => {
    loadData();
  }, [filters]);

  if (loading) {
    return (
      <div className="p-6">
        <div className="max-w-7xl mx-auto">
          <div className="mb-6">
            <Skeleton className="h-8 w-64 mb-2" />
            <Skeleton className="h-4 w-96" />
          </div>
          <div className="grid gap-6">
            <div className="grid grid-cols-1 md:grid-cols-3 gap-4">
              <Skeleton className="h-32" />
              <Skeleton className="h-32" />
              <Skeleton className="h-32" />
            </div>
            <Skeleton className="h-96" />
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="p-3">
      <div className="max-w-7xl mx-auto">
        <div className="flex items-center justify-between mb-3">
          <div>
            <h1 className="text-2xl font-bold mb-1">Utilization Dashboard</h1>
            <p className="text-muted-foreground text-sm">
              Monitor mirrord usage and analyze user activity patterns
            </p>
          </div>
        </div>

        {data && (
          <div className="space-y-4">
            <div>
              <div className="flex items-center justify-between mb-2">
                <h2 className="text-lg font-semibold">All Time Metrics</h2>
                <div className="flex-shrink-0">
                  <TimeSavedCard
                    title="Dev Time Saved"
                    value={0}
                    subtitle="based on session analysis"
                    showAdjustButton={true}
                    generalData={data.generalMetrics}
                    ciData={data.ciMetrics}
                  />
                </div>
              </div>
              <GeneralMetrics 
                generalData={data.generalMetrics} 
                ciData={data.ciMetrics} 
              />
              <div className="text-right mt-1">
                <p className="text-xs text-muted-foreground/70">
                  since {new Date('2024-01-01').toLocaleDateString('en-US', { month: 'numeric', day: 'numeric', year: 'numeric' })}
                </p>
              </div>
            </div>
            
            <div>
              <div className="flex items-center justify-between mb-2">
                <h2 className="text-lg font-semibold">Timeframe Data</h2>
                <div className="flex-shrink-0">
                  <TimeSavedCard
                    title="Dev Time Saved"
                    value={data.generalMetrics.developmentTimeSaved * 0.4}
                    subtitle="estimated for selected period"
                  />
                </div>
              </div>
              
              <div className="flex items-center justify-between mb-2">
                <div>
                  {filters.from && filters.to && (
                    <p className="text-sm text-muted-foreground">
                      {new Date(filters.from).toLocaleDateString('en-US', { month: 'short', day: 'numeric' })} - {new Date(filters.to).toLocaleDateString('en-US', { month: 'short', day: 'numeric' })}
                    </p>
                  )}
                </div>
                <div className="flex items-center gap-2">
                  <DateRangePicker filters={filters} onFiltersChange={handleFiltersChange} />
                  <Button 
                    variant="outline" 
                    size="icon"
                    onClick={loadData}
                    disabled={loading}
                  >
                    <RefreshCw className={`h-4 w-4 ${loading ? 'animate-spin' : ''}`} />
                  </Button>
                </div>
              </div>
              
              <TimeframeMetrics 
                generalData={data.generalMetrics} 
                ciData={data.ciMetrics}
                filters={filters}
              />
            </div>
            
            <UsageCharts data={data.userMetrics} />
            <UserMetricsTable data={data.userMetrics} />
          </div>
        )}
      </div>
    </div>
  );
};