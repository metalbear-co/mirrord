import { useState } from 'react'
import { ArrowRight } from 'lucide-react'
import { Button, Card, CardContent, MirrordLogo } from '@metalbear/ui'
import Wizard from './Wizard'
import { strings } from '../strings'

type WizardFlow = 'config' | 'learn' | null

const Homepage = () => {
  const [wizardOpen, setWizardOpen] = useState(false)
  const [wizardFlow, setWizardFlow] = useState<WizardFlow>(null)

  const openWizard = (flow: WizardFlow) => {
    setWizardFlow(flow)
    setWizardOpen(true)
  }

  const closeWizard = () => {
    setWizardOpen(false)
    setWizardFlow(null)
  }

  return (
    <div className="animate-fade-in mx-auto w-full max-w-md">
      <Card className="border-border overflow-hidden shadow-lg">
        {/* Decorative header */}
        <div className="bg-primary h-2" />

        <CardContent className="px-8 pb-8 pt-10">
          {/* Logo */}
          <div className="mb-8 flex justify-center">
            <div className="bg-primary/5 border-primary/10 rounded-2xl border p-4 dark:bg-[#E4E3FD]">
              <img src={MirrordLogo} alt="mirrord" className="h-12" />
            </div>
          </div>

          {/* Header */}
          <div className="mb-10 text-center">
            <h1 className="text-foreground mb-3 text-2xl font-semibold">
              {strings.homepage.title}
            </h1>
            <p className="text-muted-foreground mx-auto max-w-xs text-sm leading-relaxed">
              {strings.homepage.generatePrefix}{' '}
              <code className="bg-muted text-primary rounded px-1.5 py-0.5 text-xs font-medium">
                {strings.homepage.configFileName}
              </code>{' '}
              {strings.homepage.generateSuffix}
            </p>
          </div>

          {/* Main CTA */}
          <div className="space-y-4">
            <Button
              onClick={() => openWizard('config')}
              className="shadow-brand hover:shadow-brand-hover h-12 w-full text-base font-medium text-white transition-all duration-200"
            >
              {strings.homepage.getStarted}
              <ArrowRight className="ml-2 h-4 w-4" />
            </Button>

            <div className="relative">
              <div className="absolute inset-0 flex items-center">
                <div className="border-border w-full border-t" />
              </div>
              <div className="relative flex justify-center">
                <span className="bg-card text-muted-foreground px-3 text-xs">
                  {strings.homepage.or}
                </span>
              </div>
            </div>

            <button
              onClick={() => openWizard('learn')}
              className="text-muted-foreground hover:text-primary hover:bg-primary/5 group flex w-full items-center justify-center gap-2 rounded-lg py-3 text-center text-sm transition-colors"
            >
              <span>{strings.homepage.bookEmoji}</span>
              <span>{strings.homepage.learnBasics}</span>
            </button>
          </div>
        </CardContent>
      </Card>

      {/* Wizard Dialog */}
      <Wizard
        open={wizardOpen}
        onClose={closeWizard}
        startWithLearning={wizardFlow === 'learn'}
      />
    </div>
  )
}

export default Homepage
