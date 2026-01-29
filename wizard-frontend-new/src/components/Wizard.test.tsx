import { describe, it, expect, vi } from 'vitest';
import { render, screen, fireEvent } from '../test/test-utils';
import Wizard from './Wizard';

// Mock the sub-components to isolate Wizard tests
vi.mock('./steps/BoilerplateStep', () => ({
  default: () => <div data-testid="boilerplate-step">Boilerplate Step</div>,
}));

vi.mock('./steps/ConfigTabs', () => ({
  default: ({ currentTab, onTabChange, onCanAdvanceChange }: {
    currentTab: string;
    onTabChange: (tab: string) => void;
    onCanAdvanceChange: (canAdvance: boolean) => void;
  }) => (
    <div data-testid="config-tabs">
      <span data-testid="current-tab">{currentTab}</span>
      <button data-testid="enable-advance" onClick={() => onCanAdvanceChange(true)}>
        Enable Advance
      </button>
      <button data-testid="change-to-network" onClick={() => onTabChange('network')}>
        Go to Network
      </button>
    </div>
  ),
}));

vi.mock('./steps/LearningSteps', () => ({
  default: ({ onComplete, onSkip }: { onComplete: () => void; onSkip: () => void }) => (
    <div data-testid="learning-steps">
      <button data-testid="complete-learning" onClick={onComplete}>
        Complete Learning
      </button>
      <button data-testid="skip-learning" onClick={onSkip}>
        Skip
      </button>
    </div>
  ),
}));

vi.mock('./ErrorBoundary', () => ({
  default: ({ children }: { children: React.ReactNode }) => <>{children}</>,
}));

describe('Wizard', () => {
  const mockOnClose = vi.fn();

  beforeEach(() => {
    mockOnClose.mockClear();
  });

  describe('Basic rendering', () => {
    it('renders when open is true', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);

      expect(screen.getByText('Choose Mode')).toBeInTheDocument();
    });

    it('does not render content when open is false', () => {
      render(<Wizard open={false} onClose={mockOnClose} />);

      expect(screen.queryByText('Choose Mode')).not.toBeInTheDocument();
    });
  });

  describe('Starting with learning', () => {
    it('shows learning steps when startWithLearning is true', () => {
      render(<Wizard open={true} onClose={mockOnClose} startWithLearning={true} />);

      expect(screen.getByTestId('learning-steps')).toBeInTheDocument();
      expect(screen.getByText('Learn mirrord')).toBeInTheDocument();
    });

    it('shows boilerplate step when startWithLearning is false', () => {
      render(<Wizard open={true} onClose={mockOnClose} startWithLearning={false} />);

      expect(screen.getByTestId('boilerplate-step')).toBeInTheDocument();
    });

    it('goes to boilerplate step after completing learning', () => {
      render(<Wizard open={true} onClose={mockOnClose} startWithLearning={true} />);

      fireEvent.click(screen.getByTestId('complete-learning'));

      expect(screen.getByTestId('boilerplate-step')).toBeInTheDocument();
      expect(screen.getByText('Choose Mode')).toBeInTheDocument();
    });

    it('goes to boilerplate step when skipping learning', () => {
      render(<Wizard open={true} onClose={mockOnClose} startWithLearning={true} />);

      fireEvent.click(screen.getByTestId('skip-learning'));

      expect(screen.getByTestId('boilerplate-step')).toBeInTheDocument();
    });
  });

  describe('Navigation from boilerplate to config', () => {
    it('shows Continue button on boilerplate step', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);

      expect(screen.getByText('Continue')).toBeInTheDocument();
    });

    it('navigates to config step when Continue is clicked', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);

      fireEvent.click(screen.getByText('Continue'));

      expect(screen.getByTestId('config-tabs')).toBeInTheDocument();
      expect(screen.getByText('Configure')).toBeInTheDocument();
    });

    it('starts on target tab when entering config', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);

      fireEvent.click(screen.getByText('Continue'));

      expect(screen.getByTestId('current-tab')).toHaveTextContent('target');
    });
  });

  describe('Config step navigation', () => {
    it('shows Back button on config step', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);
      fireEvent.click(screen.getByText('Continue'));

      expect(screen.getByText('Back')).toBeInTheDocument();
    });

    it('goes back to boilerplate from config target tab', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);
      fireEvent.click(screen.getByText('Continue'));

      fireEvent.click(screen.getByText('Back'));

      expect(screen.getByTestId('boilerplate-step')).toBeInTheDocument();
    });

    it('does not show Next button when canAdvance is false', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);
      fireEvent.click(screen.getByText('Continue'));

      expect(screen.queryByText('Next')).not.toBeInTheDocument();
    });

    it('shows Next button when canAdvance is true', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);
      fireEvent.click(screen.getByText('Continue'));

      fireEvent.click(screen.getByTestId('enable-advance'));

      expect(screen.getByText('Next')).toBeInTheDocument();
    });
  });

  describe('Step indicators', () => {
    it('shows 2 step indicators when starting without learning', () => {
      render(<Wizard open={true} onClose={mockOnClose} />);

      // Find step indicators - they are divs with specific classes
      const stepDots = document.querySelectorAll('.h-2.rounded-full');
      expect(stepDots).toHaveLength(2);
    });

    it('shows 3 step indicators when learning was completed', () => {
      render(<Wizard open={true} onClose={mockOnClose} startWithLearning={true} />);

      fireEvent.click(screen.getByTestId('complete-learning'));

      const stepDots = document.querySelectorAll('.h-2.rounded-full');
      expect(stepDots).toHaveLength(3);
    });
  });

  describe('Back button in boilerplate after learning', () => {
    it('shows Back button after completing learning', () => {
      render(<Wizard open={true} onClose={mockOnClose} startWithLearning={true} />);

      fireEvent.click(screen.getByTestId('complete-learning'));

      expect(screen.getByText('Back')).toBeInTheDocument();
    });

    it('goes back to learning when clicking Back on boilerplate after learning', () => {
      render(<Wizard open={true} onClose={mockOnClose} startWithLearning={true} />);

      fireEvent.click(screen.getByTestId('complete-learning'));
      fireEvent.click(screen.getByText('Back'));

      expect(screen.getByTestId('learning-steps')).toBeInTheDocument();
    });
  });

  describe('State reset on reopen', () => {
    it('resets to initial step when wizard reopens', async () => {
      const { rerender } = render(
        <Wizard open={true} onClose={mockOnClose} startWithLearning={false} />
      );

      // Navigate to config
      fireEvent.click(screen.getByText('Continue'));
      expect(screen.getByTestId('config-tabs')).toBeInTheDocument();

      // Close and reopen
      rerender(<Wizard open={false} onClose={mockOnClose} startWithLearning={false} />);
      rerender(<Wizard open={true} onClose={mockOnClose} startWithLearning={false} />);

      // Should be back to boilerplate
      expect(screen.getByTestId('boilerplate-step')).toBeInTheDocument();
    });

    it('resets to learning step when reopening with startWithLearning', async () => {
      const { rerender } = render(
        <Wizard open={true} onClose={mockOnClose} startWithLearning={true} />
      );

      // Complete learning and go to config
      fireEvent.click(screen.getByTestId('complete-learning'));
      fireEvent.click(screen.getByText('Continue'));
      expect(screen.getByTestId('config-tabs')).toBeInTheDocument();

      // Close and reopen
      rerender(<Wizard open={false} onClose={mockOnClose} startWithLearning={true} />);
      rerender(<Wizard open={true} onClose={mockOnClose} startWithLearning={true} />);

      // Should be back to learning
      expect(screen.getByTestId('learning-steps')).toBeInTheDocument();
    });
  });
});
