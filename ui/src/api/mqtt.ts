import axios from './api';
import { useQuery } from '@tanstack/react-query';

import { MqttSettings, MqttSettingsPatch } from './schemas/mqttSchema';
export * from './schemas/mqttSchema';


export const useMqtt = () => useQuery<MqttSettings>({
  queryKey: ['useMqtt'],
  queryFn: async () => {
    const response = await axios.get<MqttSettings>('/mqtt');
    return response.data;
  },
});


export const postMqtt = (mqtt: MqttSettingsPatch) => {
  return axios.post<MqttSettings>('/mqtt', mqtt);
};
