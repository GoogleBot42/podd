import {
  Alert,
  Box,
  Button,
  CircularProgress,
  Dialog,
  DialogActions,
  DialogTitle,
  Snackbar,
  Typography,
} from '@mui/material';
import { useScheduleStore } from '../scheduleStore.tsx';
import { useAppStore } from '@state/appStore.tsx';
import { postAlarm } from '@api/alarm.ts';
import { useState } from 'react';

const TEST_DURATION_SECONDS = 10;

export default function AlarmTest() {
  const { side } = useAppStore();
  const { selectedSchedule } = useScheduleStore();
  const [isTesting, setIsTesting] = useState(false);
  const [confirmOpen, setConfirmOpen] = useState(false);
  const [testError, setTestError] = useState(false);

  const onTestAlarm = () => {
    if (!selectedSchedule) return;
    setConfirmOpen(false);

    postAlarm({
      side,
      vibrationIntensity: selectedSchedule.alarm.vibrationIntensity,
      duration: TEST_DURATION_SECONDS,
      vibrationPattern: selectedSchedule.alarm.vibrationPattern,
      force: true,
    })
      .then(() => {
        setIsTesting(true);
        // Start the countdown only once the bed has actually been told to
        // vibrate — scheduling it alongside the POST left the spinner latched
        // forever whenever the request took longer than the test itself.
        setTimeout(() => setIsTesting(false), TEST_DURATION_SECONDS * 1_000);
      })
      .catch(error => {
        console.error(error);
        setTestError(true);
      });
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
        onClick={ () => setConfirmOpen(true) }
        disabled={ isTesting || !selectedSchedule }
      >
        Test alarm
      </Button>

      { /* This vibrates a real bed, so never fire it straight off a tap. */ }
      <Dialog open={ confirmOpen } onClose={ () => setConfirmOpen(false) }>
        <DialogTitle>
          Vibrate the { side } side of the bed for { TEST_DURATION_SECONDS } seconds?
        </DialogTitle>
        <DialogActions>
          <Button onClick={ () => setConfirmOpen(false) }>Cancel</Button>
          <Button onClick={ onTestAlarm } color="error">Vibrate</Button>
        </DialogActions>
      </Dialog>

      <Snackbar
        open={ testError }
        autoHideDuration={ 6000 }
        onClose={ () => setTestError(false) }
      >
        <Alert severity="error" onClose={ () => setTestError(false) }>
          Couldn&apos;t start the test alarm
        </Alert>
      </Snackbar>
    </Box>
  );
}
