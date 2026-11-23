// The mock provider's RSA key (PKCS#1 DER). It is public on purpose: the mock lets anyone sign in as anyone,
// and in hca mode only keys from HCA's own JWKS are ever trusted.
pub const DER_B64: &str = concat!(
    "MIIEowIBAAKCAQEAlxYIJdQvbrhIoCuN5YApF2fd6lFqhB/r2bjToRJEyhVcMYLlYXjhSlWll2DpIaOy+sNLz7I7kiN2/TfU",
    "XZInv3dS7pEuHM2RYH82dfXRz5a4WcU7coWilPK3ggXvnVnz9BzKX6npLQsYRiFdziUM48dgeu6JCCRpq38ztKmCxt7Bjzpg",
    "Z6YGL3bu95h0Nx7X00UZ2uCUVYpDRBIxpch8hCAXnLmql/jv2bD28zoD2FhPb2XpoIM7Xo1RgoeDJUIW3D9WloCv8Jb3+ebR",
    "nih4VFs0bVthfdPjjFkoo3KgzwLlPP0TftISoIwfT5gqowO++q+zhYBmDMjl8z9stsubCwIDAQABAoIBAAI56Iwo0JDGFbPe",
    "exca81Vin2ojdvlUFb/ohvMZoiUuzrlKtMyMi8Kc+R/+OpEsqKd57IphQEV1UPyG2mc35G61eGWxgpzECVStqy9ucX/94uDX",
    "v3opg+1ul3ZKq27bHiAbsykDsj7f/0875fZH+iT6ZsPeSJXzy2bOggQzIeKCQfJ1pbLYmqatj6tp+/QMMZX0Qgtnh+H2fbkB",
    "E6YETCeyGsOMtLNV0Bwllj9IuXfd0U5jPSzCdoEHOgwuXUvYtyF6BNN203V3Gz94rgZd+M8SUyzu1UZHaQKGso/f1vfRN9ch",
    "q8hkN29mUQpz35o+UDxWBQPS5xf6sHep7qNe2tkCgYEA0xA9MaE/Xy2ArgGRN7lxiTOTcN7vCwrPVpLfELM/yG+zz/jnGn2a",
    "CW5EqECRrXvcFrJ2PuKRwF87D2GmJnkczVHzUdjDFrOOhOV+ofywQJeTIwc/Yf9zYsmm8XwZNualyUE5Nthy3EjyCPOXQ3s0",
    "IZON6vMyAQ4LXZxge2rYGE0CgYEAt0DML3CEH6SAXjeZpuBuwH1y62/+cz73c39p64x77tiHjUbS8DxYJ46V0u31otGsP2ZH",
    "tViumtspgDsawzX+2oABtbN+b7eJdBNHAVBZa1UXWzSfFH7QY0NCfu7dTKkhupkwORDELHVb783X4stmrytT+K9QAco4KAOF",
    "4G05LLcCgYAHUFW+hYbrpmkCHp3In412oqnmTGNeL5a1uksuifeY234Qo+BmQ5n137NTXUFvEI603qRIU5OXZfVm+AxI4wOD",
    "feZ/nJJOFBTJOCamOFpnMplvqk2g+Lgn4TL+xT0ecOmnyrHP27pHNePlOYOxW/CXhURystC1xqXQLZ4OSMqKNQKBgQCzZ9O+",
    "6ffgBqvKEcLy1ix3dRmqF8XTzCR0RZxMCAhZKFFk5Z6jpWBksbpKe78MvRtWIms3xm+CmefCa5ri3Jz6C3IhEm5JLzV5l1nf",
    "lmtEolSwbQ8FALZBR9r07CNp5vdneIv2gkycffUZC7w8hAeunMuj3vaEGhbqTbCmrCfadwKBgH6UIuNIioJ+aWsS4NIOIQva",
    "zkYy4gly1VWtbWNo5AuGMxWTe/r8BCpb1TkAndc7+78OAd3gqDSiKimuAb0AledCBzRLL71shAgzceYw4KD/y1mQ3GGvmIot",
    "ka0X/ylBQ/Z9julcujeEOeuAYOf1ghDcimQatt+7b8YiHk7pnei9",
);
pub const N: &str = "lxYIJdQvbrhIoCuN5YApF2fd6lFqhB_r2bjToRJEyhVcMYLlYXjhSlWll2DpIaOy-sNLz7I7kiN2_TfUXZInv3dS7pEuHM2RYH82dfXRz5a4WcU7coWilPK3ggXvnVnz9BzKX6npLQsYRiFdziUM48dgeu6JCCRpq38ztKmCxt7BjzpgZ6YGL3bu95h0Nx7X00UZ2uCUVYpDRBIxpch8hCAXnLmql_jv2bD28zoD2FhPb2XpoIM7Xo1RgoeDJUIW3D9WloCv8Jb3-ebRnih4VFs0bVthfdPjjFkoo3KgzwLlPP0TftISoIwfT5gqowO--q-zhYBmDMjl8z9stsubCw";
pub const E: &str = "AQAB";
pub const KID: &str = "kairos-mock-1";
