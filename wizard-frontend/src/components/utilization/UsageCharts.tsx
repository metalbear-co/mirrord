import { BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer, PieChart, Pie, Cell } from "recharts";
import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Button } from "@/components/ui/button";
import { UserMetric } from "@/types/utilization";
import { useState } from "react";

interface UsageChartsProps {
  data: UserMetric[];
}

export const UsageCharts = ({ data }: UsageChartsProps) => {
  const [sortBy, setSortBy] = useState<'sessionTime' | 'sessions'>('sessionTime');
  // Prepare data for top users chart
  const topUsersData = data
    .sort((a, b) => sortBy === 'sessionTime' ? b.totalSessionTime - a.totalSessionTime : b.sessionCount - a.sessionCount)
    .slice(0, 8)
    .map(user => ({
      name: user.kubernetesUser || user.machineId,
      sessionTime: user.totalSessionTime,
      sessions: user.sessionCount,
    }));

  // Prepare data for session distribution
  const sessionDistribution = sortBy === 'sessionTime' 
    ? [
        { name: "Light Users (0-30min)", value: data.filter(u => u.totalSessionTime <= 30).length, color: "hsl(var(--muted))" },
        { name: "Regular Users (30-90min)", value: data.filter(u => u.totalSessionTime > 30 && u.totalSessionTime <= 90).length, color: "hsl(var(--accent))" },
        { name: "Heavy Users (90min+)", value: data.filter(u => u.totalSessionTime > 90).length, color: "hsl(var(--primary))" },
      ]
    : [
        { name: "Light Users (1-5 sessions)", value: data.filter(u => u.sessionCount >= 1 && u.sessionCount <= 5).length, color: "hsl(var(--muted))" },
        { name: "Regular Users (6-15 sessions)", value: data.filter(u => u.sessionCount > 5 && u.sessionCount <= 15).length, color: "hsl(var(--accent))" },
        { name: "Heavy Users (15+ sessions)", value: data.filter(u => u.sessionCount > 15).length, color: "hsl(var(--primary))" },
      ];

  const formatDuration = (minutes: number) => {
    const hours = Math.floor(minutes / 60);
    const mins = minutes % 60;
    return hours > 0 ? `${hours}h ${mins}m` : `${mins}m`;
  };

  const CustomTooltip = ({ active, payload, label }: any) => {
    if (active && payload && payload.length) {
      const data = payload[0].payload;
      return (
        <div className="bg-card border border-border rounded-lg p-3 shadow-lg">
          <p className="font-medium">{label}</p>
          <p className="text-primary">
            {sortBy === 'sessionTime' ? `Session Time: ${formatDuration(payload[0].value)}` : `Total Sessions: ${payload[0].value}`}
          </p>
          <p className="text-accent">
            {sortBy === 'sessionTime' ? `Total Sessions: ${data.sessions}` : `Session Time: ${formatDuration(data.sessionTime)}`}
          </p>
        </div>
      );
    }
    return null;
  };

  const PieTooltip = ({ active, payload }: any) => {
    if (active && payload && payload.length) {
      return (
        <div className="bg-card border border-border rounded-lg p-3 shadow-lg">
          <p className="font-medium">{payload[0].name}</p>
          <p className="text-primary">Count: {payload[0].value}</p>
        </div>
      );
    }
    return null;
  };

  return (
    <div className="space-y-4">
      <div className="flex items-center justify-between">
        <h3 className="text-lg font-semibold">Usage Analytics</h3>
        <div className="flex gap-1">
          <Button
            variant={sortBy === 'sessionTime' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setSortBy('sessionTime')}
          >
            By Time
          </Button>
          <Button
            variant={sortBy === 'sessions' ? 'default' : 'outline'}
            size="sm"
            onClick={() => setSortBy('sessions')}
          >
            By Count
          </Button>
        </div>
      </div>
      
      <div className="grid grid-cols-1 lg:grid-cols-2 gap-6">
        <Card className="bg-gradient-card border-border/50">
          <CardHeader>
            <CardTitle>Top Active Users</CardTitle>
          </CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={300}>
              <BarChart data={topUsersData} margin={{ top: 20, right: 30, left: 20, bottom: 5 }}>
                <CartesianGrid strokeDasharray="3 3" stroke="hsl(var(--border))" />
                <XAxis 
                  dataKey="name" 
                  stroke="hsl(var(--muted-foreground))"
                  fontSize={12}
                  angle={-45}
                  textAnchor="end"
                  height={80}
                />
                <YAxis stroke="hsl(var(--muted-foreground))" fontSize={12} />
                <Tooltip content={<CustomTooltip />} />
                <Bar dataKey={sortBy} fill="hsl(var(--primary))" radius={[4, 4, 0, 0]} />
              </BarChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>

        <Card className="bg-gradient-card border-border/50">
          <CardHeader>
            <CardTitle>User Activity Distribution</CardTitle>
          </CardHeader>
          <CardContent>
            <ResponsiveContainer width="100%" height={300}>
              <PieChart>
                <Pie
                  data={sessionDistribution}
                  cx="50%"
                  cy="50%"
                  labelLine={false}
                  label={({ name, value, percent }) => `${name}: ${value} (${(percent * 100).toFixed(0)}%)`}
                  outerRadius={80}
                  fill="#8884d8"
                  dataKey="value"
                >
                  {sessionDistribution.map((entry, index) => (
                    <Cell key={`cell-${index}`} fill={entry.color} />
                  ))}
                </Pie>
                <Tooltip content={<PieTooltip />} />
              </PieChart>
            </ResponsiveContainer>
          </CardContent>
        </Card>
      </div>
    </div>
  );
};