import axios, { baseURL } from './api';
import { useQuery } from '@tanstack/react-query';
import { DeepPartial } from 'ts-essentials';
import { DeviceStatus } from './deviceStatusSchema';


export const getDeviceStatus = async () => {
  return axios.get<DeviceStatus>('/deviceStatus');
};

export const useDeviceStatus = () => useQuery<DeviceStatus>({
  queryKey: ['useDeviceStatus'],
  queryFn: async () => {
    const response = await getDeviceStatus();
    return response.data;
  },
  refetchInterval: 30_000,
});


// keepalive lets the request finish even if the page is closed right after a
// temperature change — axios/XHR would be aborted with it.
export const postDeviceStatus = async (deviceStatus: DeepPartial<DeviceStatus>) => {
  const response = await fetch(`${baseURL}/api/deviceStatus`, {
    method: 'POST',
    headers: { 'Content-Type': 'application/json' },
    body: JSON.stringify(deviceStatus),
    keepalive: true,
  });
  if (!response.ok) {
    throw new Error(`POST /deviceStatus failed: ${response.status}`);
  }
  return response;
};



