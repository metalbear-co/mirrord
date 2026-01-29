import { describe, it, expect } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { useContext } from 'react';
import {
  ConfigDataContext,
  ConfigDataContextProvider,
  DefaultConfig,
} from './UserDataContext';

describe('UserDataContext', () => {
  describe('DefaultConfig', () => {
    it('has empty target', () => {
      expect(DefaultConfig.target).toBeUndefined();
    });

    it('has mirror mode as default', () => {
      expect(DefaultConfig.feature?.network?.incoming?.mode).toBe('mirror');
    });
  });

  describe('ConfigDataContextProvider', () => {
    it('provides initial config', () => {
      const wrapper = ({ children }: { children: React.ReactNode }) => (
        <ConfigDataContextProvider>{children}</ConfigDataContextProvider>
      );

      const { result } = renderHook(() => useContext(ConfigDataContext), {
        wrapper,
      });

      expect(result.current).not.toBeNull();
      expect(result.current?.config).toBeDefined();
    });

    it('allows updating config', () => {
      const wrapper = ({ children }: { children: React.ReactNode }) => (
        <ConfigDataContextProvider>{children}</ConfigDataContextProvider>
      );

      const { result } = renderHook(() => useContext(ConfigDataContext), {
        wrapper,
      });

      const newConfig = {
        target: { path: 'deployment/test', namespace: 'default' },
      };

      act(() => {
        result.current?.setConfig(newConfig);
      });

      expect(result.current?.config.target?.path).toBe('deployment/test');
    });

    it('preserves config across re-renders', () => {
      const wrapper = ({ children }: { children: React.ReactNode }) => (
        <ConfigDataContextProvider>{children}</ConfigDataContextProvider>
      );

      const { result, rerender } = renderHook(
        () => useContext(ConfigDataContext),
        { wrapper }
      );

      const newConfig = {
        target: { path: 'deployment/api', namespace: 'staging' },
      };

      act(() => {
        result.current?.setConfig(newConfig);
      });

      rerender();

      expect(result.current?.config.target?.path).toBe('deployment/api');
      expect(result.current?.config.target?.namespace).toBe('staging');
    });
  });
});
