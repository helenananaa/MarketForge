"""Small-vector research reports do not need a large BLAS worker pool.

Configure only this Python process, before NumPy is imported. Keep explicit
operator overrides; never alter the user's global environment or other apps.
"""
import os

for variable in ('OPENBLAS_NUM_THREADS','OMP_NUM_THREADS','MKL_NUM_THREADS'):
    os.environ.setdefault(variable,'1')
