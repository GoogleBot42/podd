import { Box, Button, CircularProgress, Typography } from '@mui/material';
import { useScheduleStore } from '../scheduleStore.tsx';
import { useAppStore } from '@state/appStore.tsx';
import { postAlarm } from '@api/alarm.ts';
import { useState } from 'react';

const TEST_DURATION_SECONDS = 10;

export default function AlarmTest() {
  const { side } = useAppStore();
  const { selectedSchedule } = useScheduleStore();
  const [isTesting, setIsTesting] = useState(false);

  const onTestAlarm = () => {
    if (!selectedSchedule) return;

    postAlarm({
      side,
      vibrationIntensity: selectedSchedule.alarm.vibrationIntensity,
      duration: TEST_DURATION_SECONDS,
      vibrationPattern: selectedSchedule.alarm.vibrationPattern,
      force: true,
    })
      .then(() => {
        setIsTesting(true);
      })
      .catch(error => {
        console.error(error);
      });

    setTimeout(() => setIsTesting(false), TEST_DURATION_SECONDS * 1_000);
  };

  return (
    <Box
      sx={ {
        display: 'flex',
        flexDirection: 'row',
        alignItems: 'center',
        gap: 1,
        pr: 1,
      } }
    >
      { isTesting && (
        <Box
          sx={ {
            display: 'flex',
            flexDirection: 'row',
            alignItems: 'center',
            gap: 1,
            pl: 2
          } }
        >
          <CircularProgress size={ 12 } />
          <Typography color="textSecondary">Alarm running now...</Typography>
        </Box>
      ) }

      <Button
        variant="outlined"
        sx={ { ml: 'auto' } }
        onClick={ onTestAlarm }
        disabled={ isTesting }
      >
        Test alarm
      </Button>
    </Box>
  );
}
