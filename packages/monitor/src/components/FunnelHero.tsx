import { Button } from '@metalbear/ui'
import { ArrowRight } from 'lucide-react'

interface FunnelHeroProps {
  onConnect: () => void
}

const FEATURE_PILLS = [
  'SQS · Kafka · RabbitMQ split',
  'Postgres · MySQL · Mongo branching',
  'MirrordPolicy CRDs',
]

const SKELETON_ROW_COUNT = 6

export default function FunnelHero({ onConnect }: FunnelHeroProps) {
  return (
    <div className="relative h-full overflow-hidden">
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0"
        style={{
          backgroundImage:
            'radial-gradient(circle, hsl(var(--primary) / 0.18) 1px, transparent 1px)',
          backgroundSize: '14px 14px',
          backgroundColor: 'hsl(var(--primary) / 0.04)',
        }}
      />
      <div
        aria-hidden
        className="pointer-events-none absolute inset-0 p-7 opacity-40"
        style={{
          filter: 'blur(2.5px)',
          maskImage: 'linear-gradient(180deg, #000 0%, #000 40%, transparent 95%)',
          WebkitMaskImage: 'linear-gradient(180deg, #000 0%, #000 40%, transparent 95%)',
        }}
      >
        <FakeTeamSkeleton />
      </div>

      <div className="relative flex h-full max-w-[720px] flex-col items-start justify-center px-14 py-10">
        <div className="mb-4 flex items-center gap-2">
          <span className="text-meta text-muted-foreground font-medium">mirrord for Teams</span>
        </div>

        <h1 className="m-0 mb-3 text-3xl font-bold leading-tight tracking-tight">
          See what your team is mirroring,
          <br />
          <span className="text-primary">and a lot more</span>.
        </h1>

        <p className="text-muted-foreground m-0 mb-5 max-w-[540px] text-sm leading-relaxed">
          Install the mirrord operator on your cluster to unlock preview environments, queue
          splitting, DB branching, and platform-level policies. Plus everything we ship next.
        </p>

        <div className="mb-6 flex max-w-[540px] flex-wrap gap-1.5">
          {FEATURE_PILLS.map((label) => (
            <span
              key={label}
              className="text-meta bg-card border-border rounded-full border px-2.5 py-1 font-medium"
            >
              {label}
            </span>
          ))}
        </div>

        <div className="flex items-center gap-3">
          <Button onClick={onConnect} size="default" className="gap-1.5">
            Connect operator <ArrowRight className="h-3.5 w-3.5" />
          </Button>
          <a
            href="https://metalbear.com/mirrord/pricing?utm_source=funnel-hero&utm_medium=session-monitor"
            target="_blank"
            rel="noreferrer"
            className="text-muted-foreground hover:text-foreground text-xs transition-colors"
          >
            What unlocks →
          </a>
        </div>
      </div>
    </div>
  )
}

function FakeTeamSkeleton() {
  return (
    <div className="grid h-full grid-cols-[1fr_1.4fr] gap-5">
      <div className="flex flex-col gap-2">
        {Array.from({ length: SKELETON_ROW_COUNT }, (_, i) => (
          <div
            key={i}
            className="bg-card border-border flex h-14 items-center gap-2.5 rounded-lg border px-3"
          >
            <div className="bg-muted h-5 w-5 rounded-full" />
            <div className="flex flex-1 flex-col gap-1">
              <div className="bg-muted h-2 w-[70%] rounded" />
              <div className="bg-muted h-1.5 w-[40%] rounded opacity-70" />
            </div>
            <div className="bg-primary/30 h-3.5 w-8 rounded" />
          </div>
        ))}
      </div>
      <div className="flex flex-col gap-3">
        <div className="bg-card border-border h-20 rounded-lg border" />
        <div className="bg-card border-border h-32 rounded-lg border" />
        <div className="bg-card border-border h-48 rounded-lg border" />
      </div>
    </div>
  )
}
