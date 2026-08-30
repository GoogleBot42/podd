import { useState } from 'react';
import moment from 'moment-timezone';
import FavoriteIcon from '@mui/icons-material/Favorite';
import NavigateBeforeIcon from '@mui/icons-material/NavigateBefore';
import NavigateNextIcon from '@mui/icons-material/NavigateNext';
import { Alert, Box, Button, CircularProgress, Typography } from '@mui/material';

import Header from '../Header.tsx';
import PageContainer from '../../PageContainer.tsx';
import ErrorBoundary from '@components/ErrorBoundary.tsx';
import VitalsLineChart from '@components/VitalsLineChart.tsx';
import VitalsSummaryCard from '@components/VitalsSummaryCard.tsx';
import { useAppStore } from '@state/appStore.tsx';
import { useVitalsRecords } from '@api/vitals.ts';

/**
 * Standalone per-day vitals view, fed by podd's on-device biometrics
 * pipeline (#12). The sleep page shows the same charts scoped to an
 * analyzed night; this page works without sleep records, which podd
 * doesn't generate yet.
 */
export default function VitalsPage() {
  const { side } = useAppStore();
  const [day, setDay] = useState(moment().startOf('day'));

  const startTime = day.toISOString();
  const endTime = day.clone().add(1, 'day').toISOString();
  const isToday = day.isSame(moment().startOf('day'));

  const { data: vitalsRecords, isPending, isError } = useVitalsRecords({
    side,
    startTime,
    endTime,
  });

  return (
    <PageContainer sx={ { mb: 15, gap: 1 } }>
      <Header title="Vitals" icon={ <FavoriteIcon /> }/>

      <Box display="flex" justifyContent="space-between" alignItems="center" sx={ { mt: 1 } }>
        <Button onClick={ () => setDay(day.clone().subtract(1, 'day')) }>
          <NavigateBeforeIcon />
        </Button>
        <Typography variant="subtitle1">
          { day.format('ll') }{ isToday ? ' (today)' : '' }
        </Typography>
        <Button onClick={ () => setDay(day.clone().add(1, 'day')) } disabled={ isToday }>
          <NavigateNextIcon />
        </Button>
      </Box>

      { isPending && <CircularProgress sx={ { display: 'block', mx: 'auto', my: 4 } } /> }
      { isError && (
        <Alert severity="error">Failed to load vitals records</Alert>
      ) }
      { !isPending && !isError && !vitalsRecords?.length && (
        <Alert severity="info">
          No vitals recorded for this day yet. Records appear a few minutes
          after someone lies on the { side } side.
        </Alert>
      ) }

      { !!vitalsRecords?.length && (
        <ErrorBoundary componentName="VitalsPage">
          <VitalsSummaryCard startTime={ startTime } endTime={ endTime } />
          <VitalsLineChart vitalsRecords={ vitalsRecords } metric="heart_rate"/>
          <VitalsLineChart vitalsRecords={ vitalsRecords } metric="breathing_rate"/>
          <VitalsLineChart vitalsRecords={ vitalsRecords } metric="hrv"/>
        </ErrorBoundary>
      ) }
    </PageContainer>
  );
}
