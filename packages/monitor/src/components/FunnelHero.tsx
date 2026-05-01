import { Button } from '@metalbear/ui'
import { ArrowRight } from 'lucide-react'

interface FunnelHeroProps {
  onConnect: () => void
}

const FEATURE_PILLS = [
  'Preview environments',
  'Concurrent debugging',
  'HTTP filters',
  'SQS · Kafka · RabbitMQ split',
  'Postgres · MySQL · Mongo branching',
  'MirrordPolicy CRDs',
  'SSO + audit log',
]

export default function FunnelHero({ onConnect }: FunnelHeroProps) {
  return (
    <div className="relative h-full overflow-hidden">
      <div
        aria-hidden
        className="absolute inset-0 pointer-events-none"
        style={{
          backgroundImage:
            'radial-gradient(circle, hsl(var(--primary) / 0.18) 1px, transparent 1px)',
          backgroundSize: '14px 14px',
          backgroundColor: 'hsl(var(--primary) / 0.04)',
        }}
      />
      <div
        aria-hidden
        className="absolute inset-0 pointer-events-none p-7 opacity-40"
        style={{
          filter: 'blur(2.5px)',
          maskImage: 'linear-gradient(180deg, #000 0%, #000 40%, transparent 95%)',
          WebkitMaskImage: 'linear-gradient(180deg, #000 0%, #000 40%, transparent 95%)',
        }}
      >
        <FakeTeamSkeleton />
      </div>

      <div className="relative h-full flex flex-col items-start justify-center max-w-[720px] px-14 py-10">
        <div className="flex items-center gap-2 mb-4">
          <span className="text-meta font-semibold uppercase tracking-wider text-muted-foreground">
            mirrord for Teams
          </span>
        </div>

        <h1 className="text-3xl font-bold leading-tight tracking-tight m-0 mb-3">
          See what your team is mirroring —
          <br />
          <span className="text-primary">and a lot more</span>.
        </h1>

        <p className="text-sm leading-relaxed text-muted-foreground m-0 mb-5 max-w-[540px]">
          Install the mirrord operator on your cluster to unlock concurrent
          debugging, preview environments, queue splitting, DB branching, and
          platform-level policies — plus everything we ship next.
        </p>

        <div className="flex flex-wrap gap-1.5 mb-6 max-w-[540px]">
          {FEATURE_PILLS.map((label) => (
            <span
              key={label}
              className="text-meta font-medium px-2.5 py-1 rounded-full bg-card border border-border"
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
            href="https://metalbear.com/mirrord/pricing"
            target="_blank"
            rel="noreferrer"
            className="text-xs text-muted-foreground hover:text-foreground transition-colors"
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
    <div className="grid grid-cols-[1fr_1.4fr] gap-5 h-full">
      <div className="flex flex-col gap-2">
        {[1, 2, 3, 4, 5, 6].map((i) => (
          <div
            key={i}
            className="h-14 rounded-lg bg-card border border-border flex items-center gap-2.5 px-3"
          >
            <div className="w-5 h-5 rounded-full bg-muted" />
            <div className="flex flex-col gap-1 flex-1">
              <div className="h-2 w-[70%] rounded bg-muted" />
              <div className="h-1.5 w-[40%] rounded bg-muted opacity-70" />
            </div>
            <div className="w-8 h-3.5 rounded bg-primary/30" />
          </div>
        ))}
      </div>
      <div className="flex flex-col gap-3">
        <div className="h-20 rounded-lg bg-card border border-border" />
        <div className="h-32 rounded-lg bg-card border border-border" />
        <div className="h-48 rounded-lg bg-card border border-border" />
      </div>
    </div>
  )
}
