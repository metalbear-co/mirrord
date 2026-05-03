import { useState, useEffect, Fragment } from 'react'
import {
  Button,
  Dialog,
  DialogContent,
  DialogHeader,
  DialogTitle,
  DialogDescription,
  Separator,
} from '@metalbear/ui'
import { Check, Copy, ExternalLink, Loader2 } from 'lucide-react'
import type { OperatorWatchStatus } from '../types'

interface ConnectOperatorModalProps {
  open: boolean
  onOpenChange: (open: boolean) => void
  watchStatus: OperatorWatchStatus | null
}

const HELM_REPO_CMD = 'helm repo add metalbear https://metalbear-co.github.io/charts'
const INSTALL_CMD =
  'helm install --set license.key=<YOUR_KEY> mirrord-operator metalbear/mirrord-operator'
const VERIFY_CMD = 'mirrord operator status'

export default function ConnectOperatorModal({
  open,
  onOpenChange,
  watchStatus,
}: ConnectOperatorModalProps) {
  const [step, setStep] = useState(0)

  useEffect(() => {
    if (!open) setStep(0)
  }, [open])

  useEffect(() => {
    if (open && watchStatus?.status === 'watching') {
      const t = setTimeout(() => onOpenChange(false), 1500)
      return () => clearTimeout(t)
    }
  }, [open, watchStatus, onOpenChange])

  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent className="!max-w-[640px] sm:!max-w-[640px] !gap-0 !p-0">
        <DialogHeader className="px-6 pt-6 pb-4">
          <DialogTitle>Connect the mirrord operator</DialogTitle>
          <DialogDescription>Three steps, about 2 minutes.</DialogDescription>
          <div className="pt-4">
            <Stepper step={step} />
          </div>
        </DialogHeader>

        <Separator />

        <div className="px-6 py-5 min-h-[260px] min-w-0 overflow-hidden">
          {step === 0 && <SignupBody />}
          {step === 1 && <InstallBody />}
          {step === 2 && <VerifyBody watchStatus={watchStatus} />}
        </div>

        <Separator />

        <div className="px-6 py-3 flex items-center gap-3">
          {step > 0 ? (
            <button
              type="button"
              onClick={() => setStep(step - 1)}
              className="text-xs text-muted-foreground hover:text-foreground"
            >
              ← Back
            </button>
          ) : (
            <span />
          )}
          <div className="ml-auto flex items-center gap-3">
            {step === 0 && (
              <>
                <button
                  type="button"
                  onClick={() => setStep(1)}
                  className="text-xs text-muted-foreground hover:text-foreground"
                >
                  I already have a key →
                </button>
                <Button asChild size="sm">
                  <a
                    href="https://app.metalbear.com/?utm_source=connect-operator-modal&utm_medium=session-monitor"
                    target="_blank"
                    rel="noreferrer"
                    onClick={() => setStep(1)}
                    className="inline-flex items-center gap-1.5"
                  >
                    Open app.metalbear.com <ExternalLink className="h-3 w-3" />
                  </a>
                </Button>
              </>
            )}
            {step === 1 && (
              <Button size="sm" onClick={() => setStep(2)}>
                I&apos;ve installed it →
              </Button>
            )}
            {step === 2 && (
              <Button size="sm" onClick={() => onOpenChange(false)}>
                Done
              </Button>
            )}
          </div>
        </div>
      </DialogContent>
    </Dialog>
  )
}

function Stepper({ step }: { step: number }) {
  const labels = ['Sign up', 'Install', 'Verify']
  return (
    <div className="flex items-center justify-center gap-3 pb-3 border-b border-border mb-4">
      {labels.map((label, i) => (
        <Fragment key={label}>
          <div
            className={`flex items-center gap-1.5 text-xs font-semibold ${
              step >= i ? 'text-primary' : 'text-muted-foreground'
            }`}
          >
            <span
              className={`inline-flex items-center justify-center w-5 h-5 rounded-full text-meta font-bold ${
                step > i
                  ? 'bg-primary text-primary-foreground'
                  : step === i
                    ? 'bg-primary/15 text-primary'
                    : 'bg-muted text-muted-foreground'
              }`}
            >
              {step > i ? '✓' : i + 1}
            </span>
            {label}
          </div>
          {i < labels.length - 1 && (
            <div
              className={`w-12 h-px ${step > i ? 'bg-primary' : 'bg-border'}`}
            />
          )}
        </Fragment>
      ))}
    </div>
  )
}

function SignupBody() {
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h3 className="text-sm font-bold mb-1">Sign up at app.metalbear.com</h3>
        <p className="text-xs text-muted-foreground leading-relaxed">
          Create an account to get a license key. Your key activates the
          operator on any cluster you run mirrord against. 14-day trial, no
          card required.
        </p>
      </div>
      <ul className="flex flex-col gap-2">
        <Bullet>Queue splitting (SQS, Kafka, RabbitMQ)</Bullet>
        <Bullet>Database branching (Postgres, MySQL, Mongo)</Bullet>
        <Bullet>MirrordPolicy CRDs for platform guardrails</Bullet>
      </ul>
    </div>
  )
}

function InstallBody() {
  const rows = [
    { label: '1. Add Helm repository', cmd: HELM_REPO_CMD },
    { label: '2. Install the operator', cmd: INSTALL_CMD },
    { label: '3. Verify installation', cmd: VERIFY_CMD },
  ]
  return (
    <div className="flex flex-col gap-4 min-w-0">
      <div>
        <h3 className="text-sm font-bold mb-1">Install on your cluster</h3>
        <p className="text-xs text-muted-foreground leading-relaxed">
          Run these against your current kube context. Replace{' '}
          <code className="font-mono text-meta px-1 rounded bg-muted">
            &lt;YOUR_KEY&gt;
          </code>{' '}
          with the license from step 1.
        </p>
      </div>
      <div className="flex flex-col gap-2 min-w-0">
        {rows.map((row, i) => (
          <CommandRow key={i} label={row.label} cmd={row.cmd} />
        ))}
      </div>
      <p className="text-xs text-muted-foreground">
        Prefer Terraform or Argo CD?{' '}
        <a
          href="https://metalbear.com/mirrord/docs/managing-mirrord/operator?utm_source=connect-operator-modal&utm_medium=session-monitor"
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          See the operator install docs
        </a>
        .
      </p>
    </div>
  )
}

function VerifyBody({
  watchStatus,
}: {
  watchStatus: OperatorWatchStatus | null
}) {
  const isWatching = watchStatus?.status === 'watching'
  return (
    <div className="flex flex-col gap-4">
      <div>
        <h3 className="text-sm font-bold mb-1">Verify connection</h3>
        <p className="text-xs text-muted-foreground leading-relaxed">
          mirrord ui will detect the operator on your kube context. This
          usually takes a few seconds after install.
        </p>
      </div>
      <div className="flex items-center gap-3 px-4 py-3 rounded-lg bg-card border border-border">
        {isWatching ? (
          <Check className="h-5 w-5 text-primary" />
        ) : (
          <Loader2 className="h-5 w-5 text-primary animate-spin" />
        )}
        <div>
          <div className="text-xs font-semibold">
            {isWatching ? 'Operator detected' : 'Watching your cluster…'}
          </div>
          <div className="text-meta text-muted-foreground">
            {isWatching
              ? 'Closing this dialog now.'
              : 'Polling every 5s for the operator CRD'}
          </div>
        </div>
      </div>
      <p className="text-xs text-muted-foreground">
        Stuck?{' '}
        <a
          href="https://metalbear.com/mirrord/docs/troubleshooting/?utm_source=connect-operator-modal&utm_medium=session-monitor"
          target="_blank"
          rel="noreferrer"
          className="text-primary hover:underline"
        >
          Troubleshooting guide
        </a>
      </p>
    </div>
  )
}

function CommandRow({ label, cmd }: { label: string; cmd: string }) {
  const [copied, setCopied] = useState(false)
  return (
    <div className="min-w-0 max-w-full">
      <div className="text-meta font-semibold text-muted-foreground mb-1">
        {label}
      </div>
      <div className="relative bg-zinc-900 rounded-lg" style={{ maxWidth: '100%' }}>
        <pre
          className="m-0 p-3 pr-16 text-zinc-100 font-mono text-xs leading-relaxed overflow-x-auto whitespace-nowrap rounded-lg"
          style={{ maxWidth: '100%' }}
        >
          {cmd}
        </pre>
        <button
          type="button"
          onClick={() => {
            navigator.clipboard?.writeText(cmd)
            setCopied(true)
            setTimeout(() => setCopied(false), 1500)
          }}
          className="absolute top-1.5 right-1.5 px-2 py-1 rounded text-meta font-semibold border border-zinc-700 bg-zinc-800 text-zinc-100 hover:bg-zinc-700"
        >
          {copied ? 'Copied' : 'Copy'}
        </button>
      </div>
    </div>
  )
}

function Bullet({ children }: { children: React.ReactNode }) {
  return (
    <li className="flex gap-2 items-start text-xs">
      <span className="inline-flex items-center justify-center w-4 h-4 rounded-full bg-primary/15 text-primary text-caps font-bold flex-shrink-0 mt-0.5">
        ✓
      </span>
      <span>{children}</span>
    </li>
  )
}
