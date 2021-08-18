#[cfg(any(test, std))]
#[macro_use]
extern crate std;

use core::fmt::Debug;
use pairing::{
    group::{ff::Field, prime::PrimeCurveAffine, Curve, Group},
    Engine,
};
use thiserror::Error;

pub mod polynomial;

use polynomial::Polynomial;

/// parameters from tested setup
#[derive(Clone, Debug)]
pub struct KZGParams<E: Engine, const MAX_DEGREE: usize> {
    /// generator of g
    g: E::G1Affine,
    /// generator of G2
    h: E::G2Affine,
    /// g^alpha^1, g^alpha^2, ...
    gs: [E::G1Affine; MAX_DEGREE],
    /// g^alpha^1, g^alpha^2, ...
    hs: [E::G2Affine; MAX_DEGREE],
}

// the commitment - "C" in the paper. It's a single group element
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct KZGCommitment<E: Engine>(E::G1Affine);

// A witness for a single element - "w_i" in the paper. It's a group element.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct KZGWitness<E: Engine>(E::G1Affine);

#[derive(Error, Debug)]
pub enum KZGError {
    #[error("no polynomial!")]
    NoPolynomial,
    #[error("point not on polynomial!")]
    PointNotOnPolynomial,
}

pub struct KZGProver<E: Engine, const MAX_DEGREE: usize> {
    parameters: KZGParams<E, MAX_DEGREE>,
    polynomial: Option<Polynomial<E, MAX_DEGREE>>,
    commitment: Option<KZGCommitment<E>>,
    batch_witness: Option<E::G1>,
    witnesses: [Option<E::G1Affine>; MAX_DEGREE],
}

pub struct KZGVerifier<E: Engine, const MAX_DEGREE: usize> {
    parameters: KZGParams<E, MAX_DEGREE>,
}

impl<E: Engine, const MAX_DEGREE: usize> KZGProver<E, MAX_DEGREE> {
    /// initializes `polynomial` to zero polynomial
    fn new(parameters: KZGParams<E, MAX_DEGREE>) -> Self {
        Self {
            parameters,
            polynomial: None,
            commitment: None,
            batch_witness: None,
            witnesses: [None; MAX_DEGREE],
        }
    }

    fn commit(&mut self, polynomial: Polynomial<E, MAX_DEGREE>) -> KZGCommitment<E> {
        let mut commitment = E::G1::identity();
        for (i, &coeff) in polynomial.coeffs.iter().enumerate() {
            if i == 0 {
                commitment += self.parameters.g * coeff;
            } else {
                commitment += self.parameters.gs[i - 1] * coeff;
            }
        }

        self.polynomial = Some(polynomial);
        KZGCommitment(commitment.to_affine())
    }

    fn open(&self) -> Result<Polynomial<E, MAX_DEGREE>, KZGError> {
        self.polynomial.clone().ok_or(KZGError::NoPolynomial)
    }

    fn create_witness(&mut self, (x, y): (E::Fr, E::Fr)) -> Result<KZGWitness<E>, KZGError> {
        match self.polynomial {
            None => Err(KZGError::NoPolynomial),
            Some(ref polynomial) => {
                let mut dividend = polynomial.clone();
                dividend.coeffs[0] -= y;

                let mut divisor = Polynomial::new_from_coeffs([E::Fr::zero(); MAX_DEGREE], 1);
                divisor.coeffs[0] = -x;
                divisor.coeffs[1] = E::Fr::one();
                match dividend.long_division(&divisor) {
                    // by polynomial remainder theorem, if (x - point.x) does not divide self.polynomial, then
                    // self.polynomial(point.y) != point.1
                    (_, Some(_)) => Err(KZGError::PointNotOnPolynomial),
                    (psi, None) => {
                        let mut witness = E::G1::identity();
                        for (i, &coeff) in psi.coeffs.iter().enumerate() {
                            if i == 0 {
                                witness += self.parameters.g * coeff;
                            } else {
                                witness += self.parameters.gs[i - 1] * coeff;
                            }
                        }

                        Ok(KZGWitness(witness.to_affine()))
                    }
                }
            }
        }
    }
}

impl<E: Engine, const MAX_DEGREE: usize> KZGVerifier<E, MAX_DEGREE> {
    fn new(parameters: KZGParams<E, MAX_DEGREE>) -> Self {
        KZGVerifier { parameters }
    }

    fn verify_poly(
        &self,
        commitment: &KZGCommitment<E>,
        polynomial: &Polynomial<E, MAX_DEGREE>,
    ) -> bool {
        let mut check = E::G1::identity();
        for (i, &coeff) in polynomial.coeffs.iter().enumerate() {
            if i == 0 {
                check += self.parameters.g * coeff;
            } else {
                check += self.parameters.gs[i - 1] * coeff;
            }
        }

        check.to_affine() == commitment.0
    }

    fn verify_eval(
        &self,
        (x, y): (E::Fr, E::Fr),
        commitment: &KZGCommitment<E>,
        witness: &KZGWitness<E>,
    ) -> bool {
        let lhs = E::pairing(
            &witness.0,
            &(self.parameters.hs[0].to_curve() + self.parameters.h * -x).to_affine(),
        );
        let rhs = E::pairing(
            &(commitment.0.to_curve() - self.parameters.g * -y).to_affine(),
            &self.parameters.h,
        );

        lhs == rhs
    }
}

pub fn setup<E: Engine, const MAX_DEGREE: usize>(s: E::Fr) -> KZGParams<E, MAX_DEGREE> {
    let g = E::G1Affine::generator();
    let h = E::G2Affine::generator();

    let mut gs = [g; MAX_DEGREE];
    let mut hs = [h; MAX_DEGREE];

    let mut curr = g;

    for g in gs.iter_mut() {
        *g = (curr * s).to_affine();
        curr = *g;
    }

    let mut curr = h;
    for h in hs.iter_mut() {
        *h = (curr * s).to_affine();
        curr = *h;
    }

    KZGParams { g, h, gs, hs }
}

#[cfg(any(csprng_setup, test))]
use rand::random;

#[cfg(any(csprng_setup, test))]
pub fn csprng_setup<E: Engine, const MAX_DEGREE: usize>() -> KZGParams<E, MAX_DEGREE> {
    let s: E::Fr = random::<u64>().into();
    setup(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::{
        SeedableRng,
        Rng,
        rngs::SmallRng
    };
    use bls12_381::Bls12;
    use lazy_static::lazy_static;
    use std::sync::Mutex;

    const RNG_SEED_0: [u8; 32] = [42; 32];
    const RNG_SEED_1: [u8; 32] = [79; 32];

    lazy_static! {
        static ref RNG_0: Mutex<SmallRng> = Mutex::new(SmallRng::from_seed(RNG_SEED_0));
        static ref RNG_1: Mutex<SmallRng> = Mutex::new(SmallRng::from_seed(RNG_SEED_1));
    }

    fn test_setup<E: Engine, const MAX_DEGREE: usize>() -> KZGParams<E, MAX_DEGREE> {
        let s: E::Fr = RNG_0.lock().unwrap().gen::<u64>().into();
        setup(s)
    }

    // never returns zero polynomial
    fn random_polynomial<E: Engine, const MAX_DEGREE: usize>() -> Polynomial<E, MAX_DEGREE> {
        let degree = RNG_1.lock().unwrap().gen_range(1..MAX_DEGREE);
        let mut coeffs = [E::Fr::zero(); MAX_DEGREE];

        for i in 0..degree {
            coeffs[i] = RNG_1.lock().unwrap().gen::<u64>().into();
        }

        Polynomial::new_from_coeffs(coeffs, degree)
    }

    #[test]
    fn test_basic() {
        let params = test_setup::<Bls12, 10>();
        let mut prover = KZGProver::new(params.clone());
        let verifier = KZGVerifier::new(params);

        let mut polynomial = random_polynomial();
        let commitment = prover.commit(polynomial.clone());

        assert!(verifier.verify_poly(&commitment, &polynomial), "verify_poly failed for commitment {:#?} and polynomial {:#?}", commitment, polynomial);
        assert!(!verifier.verify_poly(&commitment, &random_polynomial()), "expected verify_poly to fail for commitment {:#?} and polynomial {:#?}", commitment, polynomial);
    }

}
