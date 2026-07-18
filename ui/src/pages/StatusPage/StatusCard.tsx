import PlayArrowIcon from '@mui/icons-material/PlayArrow';
import moment from 'moment-timezone';
import { ServerStatusKey, StatusInfo } from '@api/serverStatusSchema.ts';
import {
  Box,
  Button,
  Card,
  CardContent,
  CardHeader,
  Stack,
  Typography,
} from '@mui/material';
import Grid from '@mui/material/GridLegacy';

import StatusChip from './StatusChip.tsx';
import { postJobs, JobSchema, Jobs } from '@api/jobs.ts';
import { useState } from 'react';


type StatusCardProps = {
  statusInfo: StatusInfo;
  job: ServerStatusKey,
}
export default function StatusCard({ job, statusInfo }: StatusCardProps) {
  const timestamp = statusInfo.timestamp && moment(statusInfo.timestamp).format('YYYY-MM-DD HH:mm:ss z');
  let isRunnable = false;
  // @ts-expect-error
  if (JobSchema.options.includes(job)) {
    isRunnable = true;
  }
  const [disabled, setDisabled] = useState(false);
  const startJob = () => {
    setDisabled(true);
    postJobs([job] as Jobs)
      .catch(error => {
        console.error(error);
      });
    setTimeout(() => setDisabled(false), 30_000);
  };

  return (
    <Grid item xs={ 12 } sm={ 6 } md={ 4 }>
      <Card
        variant="outlined"
        sx={ {
          height: '100%', borderRadius: 3,
          '& .MuiCardHeader-root': { pb: 0.25 },
          '& .MuiCardContent-root': { pt: 0.75 },
        } }
      >
        <CardHeader
          title={
            <Stack direction="row" spacing={ 1.25 } alignItems="center">
              <Typography variant="subtitle1" fontWeight={ 700 }>
                { statusInfo.name }
              </Typography>
              <StatusChip info={ statusInfo }/>
            </Stack>
          }
        />
        <CardContent>
          {
            timestamp && (
              <Typography
                variant="body2"
                sx={ {
                  color: (t) => t.palette.text.secondary,
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-word',
                  minHeight: 24,
                } }
              >
                { timestamp }
              </Typography>
            )
          }
          <Typography
            variant="body2"
            sx={ {
              color: (t) => t.palette.text.secondary,
              whiteSpace: 'pre-wrap',
              wordBreak: 'break-word',
              minHeight: 24,
            } }
          >
            { statusInfo.description }
          </Typography>

          {
            statusInfo.message && (
              <Typography
                variant="body2"
                color="error"
                sx={ {
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-word',
                  minHeight: 24,
                } }
              >
                Error: { statusInfo.message }
              </Typography>
            )
          }
          {
            isRunnable && (
              <Box
                sx={ {
                  display: 'flex',
                  justifyContent: 'flex-end',
                  alignItems: 'flex-end',
                  mt: 'auto',
                  height: '100%',
                  width: '100%',
                } }
              >
                <Button onClick={ startJob } variant="contained" size="small" disabled={ disabled || statusInfo.status === 'started' }>
                  Run
                  <PlayArrowIcon/>
                </Button>
              </Box>
            )
          }
        </CardContent>
      </Card>
    </Grid>
  );
}
