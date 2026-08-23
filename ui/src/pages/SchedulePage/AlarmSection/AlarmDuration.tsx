import Box from '@mui/material/Box';
import InputLabel from '@mui/material/InputLabel';
import MenuItem from '@mui/material/MenuItem';
import FormControl from '@mui/material/FormControl';
import Select from '@mui/material/Select';
import { useAppStore } from '@state/appStore.tsx';
import { useScheduleStore } from '../scheduleStore.tsx';
import _ from 'lodash';

const DURATION_LIST = _.range(10, 190, 10);

export default function AlarmDuration() {
  const { isUpdating } = useAppStore();
  const {
    selectedSchedule,
    updateSelectedSchedule,
  } = useScheduleStore();

  // Schedules on disk can hold durations the preset list doesn't cover (179s,
  // for one). Without the stored value among the options MUI renders an empty
  // Select and warns about an out-of-range value.
  const duration = selectedSchedule?.alarm.duration;
  const durations = duration === undefined || DURATION_LIST.includes(duration)
    ? DURATION_LIST
    : _.sortBy([...DURATION_LIST, duration]);

  return (
    <Box sx={ { width: '100%' } }>
      <FormControl fullWidth>
        <InputLabel>Alarm Duration (seconds)</InputLabel>
        <Select
          disabled={ isUpdating }
          value={ duration ?? '' }
          variant='standard'
          onChange={ (event) => {
            updateSelectedSchedule(
              {
                alarm: {
                  duration: event.target.value as number,
                },
              }
            );
          } }
        >
          {
            durations.map((option) => (
              <MenuItem value={ option } key={ option }>{ option }</MenuItem>
            ))
          }
        </Select>
      </FormControl>
    </Box>
  );
}
