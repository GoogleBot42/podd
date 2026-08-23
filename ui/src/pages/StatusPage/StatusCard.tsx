import moment from 'moment-timezone';
import { StatusInfo } from '@api/serverStatusSchema.ts';
import {
  Card,
  CardContent,
  CardHeader,
  Stack,
  Typography,
} from '@mui/material';
import Grid from '@mui/material/GridLegacy';

import StatusChip from './StatusChip.tsx';


type StatusCardProps = {
  statusInfo: StatusInfo;
}
export default function StatusCard({ statusInfo }: StatusCardProps) {
  // The backend sends the time the subsystem *entered* this state.
  const since = statusInfo.timestamp
    ? moment(statusInfo.timestamp).format('YYYY-MM-DD HH:mm:ss')
    : undefined;
  const isBad = statusInfo.status === 'failed' || statusInfo.status === 'retrying';

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
                color={ isBad ? 'error' : 'text.secondary' }
                sx={ {
                  whiteSpace: 'pre-wrap',
                  wordBreak: 'break-word',
                  minHeight: 24,
                } }
              >
                { statusInfo.message }
              </Typography>
            )
          }

          {
            since && (
              <Typography
                variant="caption"
                sx={ { color: (t) => t.palette.text.disabled } }
              >
                Since { since }
              </Typography>
            )
          }
        </CardContent>
      </Card>
    </Grid>
  );
}
