import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '../../test/test-utils';
import HttpFilter from './HttpFilter';
import { ConfigDataContext } from '../UserDataContext';

describe('HttpFilter', () => {
  const mockSetConfig = vi.fn();
  const mockConfig = {
    feature: {
      network: {
        incoming: {
          mode: 'steal' as const,
          http_filter: {
            header: 'x-test: value',
          },
        },
      },
    },
  };

  const renderWithContext = (initValue = 'x-test: value', inputType: 'header' | 'path' = 'header') => {
    return render(
      <ConfigDataContext.Provider value={{ config: mockConfig, setConfig: mockSetConfig }}>
        <HttpFilter initValue={initValue} inputType={inputType} />
      </ConfigDataContext.Provider>
    );
  };

  beforeEach(() => {
    mockSetConfig.mockClear();
  });

  it('displays the filter value', () => {
    renderWithContext();

    expect(screen.getByDisplayValue('x-test: value')).toBeInTheDocument();
  });

  it('displays regex wrapper slashes', () => {
    renderWithContext();

    // Check for the regex wrapper characters
    const container = screen.getByDisplayValue('x-test: value').closest('div');
    expect(container?.textContent).toContain('/');
  });

  it('renders the delete button', () => {
    renderWithContext();

    const deleteButton = screen.getByRole('button');
    expect(deleteButton).toBeInTheDocument();
  });

  it('calls setConfig when delete button is clicked', () => {
    renderWithContext();

    const deleteButton = screen.getByRole('button');
    fireEvent.click(deleteButton);

    expect(mockSetConfig).toHaveBeenCalled();
  });

  it('input is read-only', () => {
    renderWithContext();

    const input = screen.getByDisplayValue('x-test: value') as HTMLInputElement;
    expect(input.readOnly).toBe(true);
  });

  it('works with path filter type', () => {
    renderWithContext('/api/users', 'path');

    expect(screen.getByDisplayValue('/api/users')).toBeInTheDocument();
  });
});
