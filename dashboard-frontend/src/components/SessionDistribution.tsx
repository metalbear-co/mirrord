import { PieChart, Pie, Cell, ResponsiveContainer, Tooltip, Legend } from 'recharts';
import { FONT_SIZE_BODY_SM } from '@metalbear/ui';
import { getChartColors } from '@/lib/theme';

interface SessionDistributionProps {
  mirrorCount: number;
  stealCount: number;
  isDarkMode: boolean;
}

export function SessionDistribution({
  mirrorCount,
  stealCount,
  isDarkMode,
}: SessionDistributionProps) {
  const colors = getChartColors(isDarkMode);

  const data = [
    { name: 'Steal', value: stealCount, color: colors.primary },
    { name: 'Mirror', value: mirrorCount, color: colors.secondary },
  ];

  const total = mirrorCount + stealCount;

  return (
    <div className="card">
      <h3 className="text-h4 font-semibold text-[var(--foreground)] mb-4">
        Session Mode Distribution
      </h3>
      <div className="h-[300px]">
        <ResponsiveContainer width="100%" height="100%">
          <PieChart>
            <Pie
              data={data}
              cx="50%"
              cy="50%"
              innerRadius={60}
              outerRadius={100}
              paddingAngle={2}
              dataKey="value"
              labelLine={false}
              label={({ name, percent, cx, cy, midAngle, outerRadius }) => {
                const RADIAN = Math.PI / 180;
                const radius = outerRadius + 25;
                const x = cx + radius * Math.cos(-midAngle * RADIAN);
                const y = cy + radius * Math.sin(-midAngle * RADIAN);
                return (
                  <text
                    x={x}
                    y={y}
                    fill={colors.text}
                    textAnchor={x > cx ? 'start' : 'end'}
                    dominantBaseline="central"
                    fontSize={FONT_SIZE_BODY_SM}
                  >
                    {`${name} ${(percent * 100).toFixed(0)}%`}
                  </text>
                );
              }}
            >
              {data.map((entry, index) => (
                <Cell key={`cell-${index}`} fill={entry.color} />
              ))}
            </Pie>
            <Tooltip
              contentStyle={{
                backgroundColor: colors.tooltipBg,
                border: `1px solid ${colors.tooltipBorder}`,
                borderRadius: '8px',
              }}
              itemStyle={{ color: colors.text }}
              labelStyle={{ color: colors.text }}
              formatter={(value: number) => [
                `${value} sessions (${((value / total) * 100).toFixed(1)}%)`,
                '',
              ]}
            />
            <Legend
              verticalAlign="bottom"
              height={36}
              formatter={(value) => <span style={{ color: colors.axis }}>{value}</span>}
            />
          </PieChart>
        </ResponsiveContainer>
      </div>
      <div className="flex justify-around mt-4 pt-4 border-t border-[var(--border)]">
        <div className="text-center">
          <p className="text-h4 font-bold text-primary">{stealCount}</p>
          <p className="text-body-sm text-muted">Steal Sessions</p>
        </div>
        <div className="text-center">
          <p className="text-h4 font-bold text-[var(--foreground)]">{mirrorCount}</p>
          <p className="text-body-sm text-muted">Mirror Sessions</p>
        </div>
      </div>
    </div>
  );
}
