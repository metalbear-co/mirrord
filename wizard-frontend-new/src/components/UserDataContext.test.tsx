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
      const network = DefaultConfig.feature?.network;
      // network can be boolean or object
      const incoming = typeof network === 'object' && network !== null && 'incoming' in network
        ? network.incoming
        : undefined;
      // incoming can also be boolean or object with mode
      const mode = typeof incoming === 'object' && incoming !== null && 'mode' in incoming
        ? incoming.mode
        : undefined;
      expect(mode).toBe('mirror');
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

      const target = result.current?.config.target;
      const path = typeof target === 'object' && target !== null && 'path' in target
        ? target.path
        : undefined;
      expect(path).toBe('deployment/test');
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

      const target = result.current?.config.target;
      const path = typeof target === 'object' && target !== null && 'path' in target
        ? target.path
        : undefined;
      const namespace = typeof target === 'object' && target !== null && 'namespace' in target
        ? target.namespace
        : undefined;
      expect(path).toBe('deployment/api');
      expect(namespace).toBe('staging');
    });
  });
});
