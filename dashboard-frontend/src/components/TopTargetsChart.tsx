import { BarChart, Bar, XAxis, YAxis, CartesianGrid, Tooltip, ResponsiveContainer } from 'recharts';
import { FONT_SIZE_BODY_SM } from '@metalbear/ui';
import { getChartColors } from '@/lib/theme';
import { getTopItems } from '@/lib/utils';

interface TopTargetsChartProps {
  sessionsByTarget: Record<string, number>;
  isDarkMode: boolean;
}

export function TopTargetsChart({ sessionsByTarget, isDarkMode }: TopTargetsChartProps) {
  const data = getTopItems(sessionsByTarget, 6);
  const colors = getChartColors(isDarkMode);

  return (
    <div className="card">
      <h3 className="text-h4 font-semibold text-[var(--foreground)] mb-4">Top Targets</h3>
      <div className="h-[300px]">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart
            data={data}
            layout="vertical"
            margin={{ top: 5, right: 20, left: 0, bottom: 5 }}
          >
            <CartesianGrid
              strokeDasharray="3 3"
              stroke={colors.grid}
              horizontal={true}
              vertical={false}
            />
            <XAxis
              type="number"
              stroke={colors.axis}
              fontSize={FONT_SIZE_BODY_SM}
              tickLine={false}
              axisLine={{ stroke: colors.grid }}
            />
            <YAxis
              type="category"
              dataKey="name"
              stroke={colors.axis}
              fontSize={FONT_SIZE_BODY_SM}
              tickLine={false}
              axisLine={{ stroke: colors.grid }}
              width={120}
            />
            <Tooltip
              contentStyle={{
                backgroundColor: colors.tooltipBg,
                border: `1px solid ${colors.tooltipBorder}`,
                borderRadius: '8px',
                color: colors.text,
              }}
              formatter={(value: number) => [`${value} sessions`, 'Sessions']}
            />
            <Bar dataKey="value" fill={colors.primary} radius={[0, 4, 4, 0]} barSize={24} />
          </BarChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}

interface UserActivityChartProps {
  sessionsByUser: Record<string, number>;
  isDarkMode: boolean;
}

export function UserActivityChart({ sessionsByUser, isDarkMode }: UserActivityChartProps) {
  const data = getTopItems(sessionsByUser, 8);
  const colors = getChartColors(isDarkMode);

  return (
    <div className="card">
      <h3 className="text-h4 font-semibold text-[var(--foreground)] mb-4">User Activity</h3>
      <div className="h-[300px]">
        <ResponsiveContainer width="100%" height="100%">
          <BarChart data={data} margin={{ top: 5, right: 20, left: 0, bottom: 5 }}>
            <CartesianGrid strokeDasharray="3 3" stroke={colors.grid} vertical={false} />
            <XAxis
              dataKey="name"
              stroke={colors.axis}
              fontSize={FONT_SIZE_BODY_SM}
              tickLine={false}
              axisLine={{ stroke: colors.grid }}
            />
            <YAxis
              stroke={colors.axis}
              fontSize={FONT_SIZE_BODY_SM}
              tickLine={false}
              axisLine={{ stroke: colors.grid }}
            />
            <Tooltip
              contentStyle={{
                backgroundColor: colors.tooltipBg,
                border: `1px solid ${colors.tooltipBorder}`,
                borderRadius: '8px',
                color: colors.text,
              }}
              formatter={(value: number) => [`${value} sessions`, 'Sessions']}
            />
            <Bar dataKey="value" fill={colors.primary} radius={[4, 4, 0, 0]} barSize={32} />
          </BarChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
