import {
  LineChart,
  Line,
  XAxis,
  YAxis,
  CartesianGrid,
  Tooltip,
  Legend,
  ResponsiveContainer,
} from 'recharts';
import { format, parseISO } from 'date-fns';
import { FONT_SIZE_BODY_SM } from '@metalbear/ui';
import { getChartColors } from '@/lib/theme';
import { strings } from '@/lib/strings';
import type { UsageDataPoint } from '@/types/mirrord';

interface UsageChartProps {
  data: UsageDataPoint[];
  isDarkMode: boolean;
}

export function UsageChart({ data, isDarkMode }: UsageChartProps) {
  const formattedData = data.map((point) => ({
    ...point,
    formattedDate: format(parseISO(point.date), 'MMM d'),
  }));

  const colors = getChartColors(isDarkMode);

  return (
    <div className="card">
      <h3 className="text-h4 font-semibold text-[var(--foreground)] mb-4">
        {strings.charts.usageChart.title}
      </h3>
      <div className="h-[300px]">
        <ResponsiveContainer width="100%" height="100%">
          <LineChart data={formattedData} margin={{ top: 5, right: 20, left: 0, bottom: 5 }}>
            <CartesianGrid strokeDasharray="3 3" stroke={colors.grid} />
            <XAxis
              dataKey="formattedDate"
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
              labelStyle={{ color: colors.axis }}
            />
            <Legend />
            <Line
              type="monotone"
              dataKey="sessions"
              name={strings.charts.usageChart.sessions}
              stroke={colors.primary}
              strokeWidth={2}
              dot={false}
              activeDot={{ r: 6, fill: colors.primary }}
            />
            <Line
              type="monotone"
              dataKey="users"
              name={strings.charts.usageChart.activeUsers}
              stroke={colors.secondary}
              strokeWidth={2}
              dot={false}
              activeDot={{ r: 6, fill: colors.secondary }}
            />
          </LineChart>
        </ResponsiveContainer>
      </div>
    </div>
  );
}
