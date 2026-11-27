use super::*;

#[test]
fn base_save_shape_requires_real_fpen_and_strict_extended_fields() {
    for fp in 0..4 {
        for sve in 0..4 {
            for sme in 0..4 {
                let bits = (fp << 20) | (sve << 16) | (sme << 24) | (1 << 28);
                assert_eq!(base_simd(bits), matches!(fp, 1 | 3) && sve == 0 && sme == 0);
            }
        }
    }
}
