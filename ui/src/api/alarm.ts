import axios from './api';
import type { AlarmJob } from './schemas/schedulesSchema';


export const postAlarm = (alarmJob: AlarmJob) => {
  return axios.post('/alarm', alarmJob);
};
