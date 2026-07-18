import axios from './api';
import { useQuery } from '@tanstack/react-query';
import { MovementRecord } from './schemas/movementRecordSchema';
export type { MovementRecord };

interface MovementQueryParams {
  startTime?: string; // ISO 8601 format (e.g., 2025-01-01T00:00:00Z)
  endTime?: string; // ISO 8601 format (e.g., 2025-01-31T23:59:59Z)
  side?: 'left' | 'right';
}


export const useMovementRecords = (params?: MovementQueryParams, enabled=true) => {
  return useQuery<MovementRecord[]>({
    queryKey: ['useMovementRecords', params],
    queryFn: async () => {
      const queryParams = new URLSearchParams();

      if (params?.startTime) queryParams.append('startTime', params.startTime);
      if (params?.endTime) queryParams.append('endTime', params.endTime);
      if (params?.side) queryParams.append('side', params.side);

      const response = await axios.get<MovementRecord[]>(`/metrics/movement?${queryParams.toString()}`);
      return response.data;
    },
    enabled,
  });
};



