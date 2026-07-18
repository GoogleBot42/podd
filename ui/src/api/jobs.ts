import axios from './api';
import { Jobs } from './schemas/jobsSchema';
export * from './schemas/jobsSchema';


export const postJobs = (jobs: Jobs) => {
  return axios.post('/jobs', jobs);
};
