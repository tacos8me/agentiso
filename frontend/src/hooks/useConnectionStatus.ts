import { useState, useEffect } from 'react';
import { getConnectionStatus, onConnectionStatusChange } from '../api/realtimeSync';

type ConnectionStatus = 'connected' | 'disconnected' | 'reconnecting';

export function useConnectionStatus(): ConnectionStatus {
  const [status, setStatus] = useState<ConnectionStatus>(getConnectionStatus);

  useEffect(() => {
    return onConnectionStatusChange(setStatus);
  }, []);

  return status;
}
