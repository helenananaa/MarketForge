"""Validate completed native output without loading its large arrays in Python."""
import json
from pathlib import Path
from research_market_mechanisms import sha


def finish_result(result, receipt, row, summary_path=None):
    receipt = Path(receipt)
    if result['exit_code'] == 0 and not result.get('failure'):
        try:
            if summary_path is not None:
                summary_path = Path(summary_path)
                if summary_path.stat().st_size > 1024*1024:
                    raise ValueError('Native completion summary exceeds 1 MiB')
                data = json.loads(summary_path.read_text(encoding='utf-8'))
                receipt_digest = sha(receipt)
                if data['schema'] != 1 or data['receipt_sha256'] != receipt_digest:
                    raise ValueError('Native completion summary/receipt hash mismatch')
                for archive in data.get('execution_archives', []):
                    if not isinstance(archive['records'], int) or archive['records'] < 0:
                        raise ValueError('Invalid execution archive count')
                    if sha(Path(archive['path'])) != archive['sha256']:
                        raise ValueError('Execution archive hash mismatch')
            else:
                # Compatibility for small old fixtures. Large historical receipts
                # must be processed with their frozen launcher, not rematerialized.
                if receipt.stat().st_size > 4*1024*1024:
                    raise ValueError('Large receipt requires a native completion summary')
                data = json.loads(receipt.read_text(encoding='utf-8'))
                receipt_digest = sha(receipt)
            valid = (data['steps'] == row['steps']
                     and data['clock_step_seconds'] == row['seconds']
                     and data['simulation_time_ms'] == row['steps']*row['seconds']*1000
                     and data.get('run_complete', True) is True)
            if not valid:
                raise ValueError('Receipt does not cover requested clock/horizon')
            result.update(complete=True, trades=data['trades'], receipt_sha256=receipt_digest,
                          simulation_time_ms=data['simulation_time_ms'])
        except (ValueError, KeyError, OSError, TypeError) as error:
            result.update(complete=False, failure='Invalid or incomplete native receipt: '+str(error))
    if not result.get('complete'):
        result.update(complete=False, partial_receipt_bytes=receipt.stat().st_size,
                      partial_receipt_sha256=sha(receipt))
    return result
