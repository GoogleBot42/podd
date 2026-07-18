import axios from './api';
import { useQuery } from '@tanstack/react-query';

// WARNING! - Any changes here MUST be the same between app/src/api & server/src/db/
import { Services } from './schemas/servicesSchema';
import { DeepPartial } from 'ts-essentials';
import { closeSentry, initSentry } from '../sentry.tsx';
export * from './schemas/servicesSchema';


export const useServices = () => {
  return useQuery<Services>({
    queryKey: ['useServices'],
    queryFn: async () => {
      const response = await axios.get<Services>('/services');
      return response.data;
    },
  });
};

export const postServices = (services: DeepPartial<Services>) => {
  if (services?.sentryLogging?.enabled === false) {
    closeSentry();
  } else {
    initSentry();
  }
  return axios.post('/services', services);
};

