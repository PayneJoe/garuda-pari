mod bench;
pub mod transcript;
use ark_ec::VariableBaseMSM;
use ark_ff::{BigInteger, Field, PrimeField};
pub use bench::BenchResult;

/// Takes as input a struct, and converts them to a series of bytes. All traits
/// that implement `CanonicalSerialize` can be automatically converted to bytes
/// in this manner.
#[macro_export]
macro_rules! to_bytes {
    ($x:expr) => {{
        let mut buf = ark_std::vec![];
        ark_serialize::CanonicalSerialize::serialize_compressed($x, &mut buf).map(|_| buf)
    }};
}

// Compute msm using windowed non-adjacent form
pub fn msm_bigint_wnaf<V: VariableBaseMSM>(
    bases: &[V::MulBase],
    scalars: &[<V::ScalarField as PrimeField>::BigInt],
) -> V {
    const C: usize = 2;
    let digits_count = const { (V::ScalarField::MODULUS_BIT_SIZE as usize).div_ceil(C) };
    let radix: u64 = 1 << C;
    let scalar_digits = scalars
        .iter()
        .flat_map(|s| make_digits::<C>(s, digits_count, radix))
        .collect::<Vec<_>>();
    let zero = V::zero();
    let mut window_sums = (0..digits_count).map(|i| {
        let mut buckets = [zero; 1 << C];
        for (digits, base) in scalar_digits.chunks(digits_count).zip(bases) {
            use ark_std::cmp::Ordering;
            // digits is the digits thing of the first scalar?
            let scalar = digits[i];
            match 0.cmp(&scalar) {
                Ordering::Less => buckets[(scalar - 1) as usize] += base,
                Ordering::Greater => buckets[(-scalar - 1) as usize] -= base,
                Ordering::Equal => (),
            }
        }

        let mut running_sum = V::zero();
        let mut res = V::zero();
        buckets.into_iter().rev().for_each(|b| {
            running_sum += &b;
            res += &running_sum;
        });
        res
    });

    // We store the sum for the lowest window.
    let lowest = window_sums.next().unwrap();

    // We're traversing windows from high to low.
    lowest
        + &window_sums.rev().fold(zero, |mut total, sum_i| {
            total += sum_i;
            for _ in 0..C {
                total.double_in_place();
            }
            total
        })
}

// From: https://github.com/arkworks-rs/gemini/blob/main/src/kzg/msm/variable_base.rs#L20
#[inline]
fn make_digits<const W: usize>(
    a: &impl BigInteger,
    digits_count: usize,
    radix: u64,
) -> impl Iterator<Item = i64> + '_ {
    let scalar = a.as_ref();
    let window_mask: u64 = radix - 1;

    let mut carry = 0u64;
    (0..digits_count).map(move |i| {
        // Construct a buffer of bits of the scalar, starting at `bit_offset`.
        let bit_offset = i * W;
        let u64_idx = bit_offset / 64;
        let bit_idx = bit_offset % 64;
        // Read the bits from the scalar
        let scalar_at_idx = scalar[u64_idx];
        let bit_buf = if bit_idx < 64 - W || u64_idx == scalar.len() - 1 {
            // This window's bits are contained in a single u64,
            // or it's the last u64 anyway.
            scalar_at_idx >> bit_idx
        } else {
            let scalar_at_idx_next = scalar[1 + u64_idx];
            // Combine the current u64's bits with the bits from the next u64
            (scalar_at_idx >> bit_idx) | (scalar_at_idx_next << (64 - bit_idx))
        };

        // Read the actual coefficient value from the window
        let coef = carry + (bit_buf & window_mask); // coef = [0, 2^r)

        // Recenter coefficients from [0,2^w) to [-2^w/2, 2^w/2)
        carry = (coef + radix / 2) >> W;
        let mut digit = (coef as i64) - (carry << W) as i64;

        if i == digits_count - 1 {
            digit += (carry << W) as i64;
        }
        digit
    })
}

/// Given a vector of field elements {v_i}, compute the vector {coeff * v_i^(-1)}.
/// This method is explicitly single-threaded.
pub fn batch_inversion_and_mul<F: Field>(v: &mut [F], coeff: &F) {
    // Montgomery’s Trick and Fast Implementation of Masked AES
    // Genelle, Prouff and Quisquater
    // Section 3.2
    // but with an optimization to multiply every element in the returned vector by
    // coeff

    // First pass: compute [a, ab, abc, ...]
    let mut prod = Vec::with_capacity(v.len());
    let mut tmp = F::one();
    for f in v.iter().filter(|f| !f.is_zero()) {
        tmp *= f;
        prod.push(tmp);
    }

    // Invert `tmp`.
    tmp = tmp.inverse().unwrap(); // Guaranteed to be nonzero.

    // Multiply product by coeff, so all inverses will be scaled by coeff
    tmp *= coeff;

    // Second pass: iterate backwards to compute inverses
    for (mut f, s) in v
        .iter_mut()
        // Backwards
        .rev()
        // Ignore normalized elements
        .filter(|f| !f.is_zero())
        // Backwards, skip last element, fill in one for last term.
        .zip(prod.into_iter().rev().skip(1).chain(Some(F::one())))
    {
        // tmp := tmp * f; f := tmp * s = 1/f
        let new_tmp = tmp * *f;
        *f = tmp * &s;
        tmp = new_tmp;
    }
}
