import axios from './api';
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


export const postDeviceStatus = (deviceStatus: DeepPartial<DeviceStatus>) => {
  return axios.post('/deviceStatus', deviceStatus);
};



